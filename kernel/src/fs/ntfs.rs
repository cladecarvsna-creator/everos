//! NTFS, the file system Windows is installed on, read only. EverOS can
//! open the files and folders on an NTFS disk but never writes to it, so
//! a Windows disk attached to EverOS can't be damaged.
//!
//! An NTFS volume keeps every file as a record in the Master File Table
//! ($MFT). A record holds attributes: the file's names, its data (small
//! data right in the record, big data as runs of clusters elsewhere on
//! the disk) and, for folders, an index of the names inside, sorted in a
//! B-tree of 4 KiB blocks.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::ata::Ata;
use super::{same_name, Error, Info};

const SECTOR: usize = 512;

const AT_ATTRIBUTE_LIST: u32 = 0x20;
const AT_VOLUME_NAME: u32 = 0x60;
const AT_DATA: u32 = 0x80;
const AT_INDEX_ROOT: u32 = 0x90;
const AT_INDEX_ALLOCATION: u32 = 0xa0;
const AT_BITMAP: u32 = 0xb0;
const AT_END: u32 = 0xffff_ffff;

/// Attribute flags.
const COMPRESSED: u16 = 0x0001;
const ENCRYPTED: u16 = 0x4000;

/// File name flags (like FILE_ATTRIBUTE_*).
const HIDDEN: u32 = 0x02;
const SYSTEM: u32 = 0x04;
const IS_DIR: u32 = 0x1000_0000;

/// The records every volume has.
const VOLUME_RECORD: u64 = 3;
const ROOT_RECORD: u64 = 5;

/// Files bigger than this are not read into memory.
pub const MAX_READ: u64 = 48 * 1024 * 1024;
/// MFT records kept in memory.
const CACHE_RECORDS: usize = 2048;
/// Folder listings kept in memory.
const CACHE_DIRS: usize = 16;

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

fn utf16(b: &[u8]) -> String {
    char::decode_utf16(b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)))
        .map(|c| c.unwrap_or('\u{fffd}'))
        .collect()
}

/// Whether a boot sector is NTFS's.
pub fn is_ntfs(b: &[u8]) -> bool {
    b.len() >= SECTOR && &b[3..11] == b"NTFS    " && b[510] == 0x55 && b[511] == 0xaa
}

/// Where the NTFS partitions on a disk start: the whole disk, MBR
/// partitions and GPT partitions are all looked at.
pub fn find_partitions(disk: &mut Ata) -> Vec<u64> {
    let mut out = Vec::new();
    let mut s0 = [0u8; SECTOR];
    if disk.read(0, &mut s0).is_err() {
        return out;
    }
    if is_ntfs(&s0) {
        out.push(0);
        return out;
    }
    if s0[510..] != [0x55, 0xaa] {
        return out;
    }
    let mut starts = Vec::new();
    for i in 0..4 {
        let p = &s0[446 + i * 16..462 + i * 16];
        match p[4] {
            0x07 | 0x17 | 0x27 => starts.push(u32_at(p, 8) as u64),
            0xee => {
                // GPT: the header is in sector 1
                let mut h = [0u8; SECTOR];
                if disk.read(1, &mut h).is_err() || &h[..8] != b"EFI PART" {
                    continue;
                }
                let first = u64_at(&h, 72);
                let count = u32_at(&h, 80).min(128) as usize;
                let size = u32_at(&h, 84) as usize;
                if !(128..=512).contains(&size) {
                    continue;
                }
                let sectors = (count * size).div_ceil(SECTOR);
                let mut table = vec![0u8; sectors * SECTOR];
                if disk.read(first, &mut table).is_err() {
                    continue;
                }
                for e in table.chunks(size).take(count) {
                    if e[..16].iter().any(|&b| b != 0) {
                        starts.push(u64_at(e, 32));
                    }
                }
            }
            _ => {}
        }
    }
    for start in starts {
        let mut b = [0u8; SECTOR];
        if start > 0 && disk.read(start, &mut b).is_ok() && is_ntfs(&b) {
            out.push(start);
        }
    }
    out
}

/// A piece of an attribute's data: `len` clusters from cluster `vcn` of
/// the data are at cluster `lcn` of the volume, or are zeros (sparse).
#[derive(Clone, Copy, Debug)]
struct Run {
    vcn: u64,
    lcn: Option<u64>,
    len: u64,
}

fn decode_runs(b: &[u8], start_vcn: u64) -> Vec<Run> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut vcn = start_vcn;
    let mut lcn: i64 = 0;
    while i < b.len() && b[i] != 0 {
        let h = b[i];
        let len_bytes = (h & 15) as usize;
        let off_bytes = (h >> 4) as usize;
        i += 1;
        if len_bytes == 0 || len_bytes > 8 || off_bytes > 8 || i + len_bytes + off_bytes > b.len() {
            break;
        }
        let mut len = 0u64;
        for k in 0..len_bytes {
            len |= (b[i + k] as u64) << (8 * k);
        }
        i += len_bytes;
        if off_bytes == 0 {
            out.push(Run {
                vcn,
                lcn: None,
                len,
            });
        } else {
            let mut d = 0i64;
            for k in 0..off_bytes {
                d |= (b[i + k] as i64) << (8 * k);
            }
            // sign-extend
            let shift = 64 - 8 * off_bytes as u32;
            d = (d << shift) >> shift;
            i += off_bytes;
            lcn += d;
            out.push(Run {
                vcn,
                lcn: Some(lcn as u64),
                len,
            });
        }
        vcn += len;
    }
    out
}

/// Undo the "update sequence" NTFS writes over the last two bytes of
/// every sector of a record, which lets it notice torn writes.
fn fixup(buf: &mut [u8], magic: &[u8; 4]) -> bool {
    if buf.len() < 8 || &buf[..4] != magic {
        return false;
    }
    let usa = u16_at(buf, 4) as usize;
    let n = u16_at(buf, 6) as usize;
    if n == 0 || usa + n * 2 > buf.len() {
        return false;
    }
    let check = [buf[usa], buf[usa + 1]];
    for k in 1..n {
        let end = k * SECTOR - 2;
        if end + 2 > buf.len() {
            break;
        }
        if buf[end..end + 2] != check {
            return false;
        }
        buf[end] = buf[usa + 2 * k];
        buf[end + 1] = buf[usa + 2 * k + 1];
    }
    true
}

#[derive(Clone)]
enum Body {
    Resident(Vec<u8>),
    NonResident {
        start_vcn: u64,
        runs: Vec<Run>,
        size: u64,
        initialized: u64,
        /// Compression unit: 2^unit clusters, or 0.
        unit: u8,
    },
}

#[derive(Clone)]
struct Attr {
    kind: u32,
    name: String,
    flags: u16,
    body: Body,
}

fn parse_attrs(rec: &[u8]) -> Vec<Attr> {
    let mut out = Vec::new();
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());
    let mut off = u16_at(rec, 0x14) as usize;
    while off + 16 <= used {
        let kind = u32_at(rec, off);
        if kind == AT_END {
            break;
        }
        let len = u32_at(rec, off + 4) as usize;
        if len < 16 || off + len > used {
            break;
        }
        let a = &rec[off..off + len];
        let name_len = a[9] as usize;
        let name_off = u16_at(a, 10) as usize;
        let name = if name_len > 0 && name_off + name_len * 2 <= len {
            utf16(&a[name_off..name_off + name_len * 2])
        } else {
            String::new()
        };
        let flags = u16_at(a, 12);
        let body = if a[8] == 0 {
            let vlen = u32_at(a, 0x10) as usize;
            let voff = u16_at(a, 0x14) as usize;
            if voff + vlen > len {
                off += len;
                continue;
            }
            Body::Resident(a[voff..voff + vlen].to_vec())
        } else {
            if len < 0x40 {
                off += len;
                continue;
            }
            let start_vcn = u64_at(a, 0x10);
            let runs_off = (u16_at(a, 0x20) as usize).min(len);
            Body::NonResident {
                start_vcn,
                runs: decode_runs(&a[runs_off..], start_vcn),
                size: u64_at(a, 0x30),
                initialized: u64_at(a, 0x38),
                unit: a[0x22],
            }
        };
        out.push(Attr {
            kind,
            name,
            flags,
            body,
        });
        off += len;
    }
    out
}

/// One attribute out of all a file has, its pieces from several records
/// joined into one.
fn pick(attrs: &[Attr], kind: u32, name: &str) -> Option<Attr> {
    let mut parts: Vec<&Attr> = attrs
        .iter()
        .filter(|a| a.kind == kind && same_name(&a.name, name))
        .collect();
    let first = (*parts.first()?).clone();
    if parts.len() == 1 || matches!(first.body, Body::Resident(_)) {
        return Some(first);
    }
    parts.sort_by_key(|a| match a.body {
        Body::NonResident { start_vcn, .. } => start_vcn,
        _ => 0,
    });
    let mut joined = parts[0].clone();
    if let Body::NonResident { runs, .. } = &mut joined.body {
        for p in &parts[1..] {
            if let Body::NonResident { runs: more, .. } = &p.body {
                runs.extend_from_slice(more);
            }
        }
    }
    Some(joined)
}

/// A name in a folder's index.
#[derive(Clone)]
struct Name {
    record: u64,
    name: String,
    flags: u32,
    size: u64,
    modified: u64,
}

fn parse_name(record: u64, key: &[u8]) -> Option<Name> {
    if key.len() < 0x42 {
        return None;
    }
    let len = key[0x40] as usize;
    let namespace = key[0x41];
    if namespace == 2 || 0x42 + len * 2 > key.len() {
        return None; // the 8.3 DOS name; the long one is listed too
    }
    Some(Name {
        record,
        name: utf16(&key[0x42..0x42 + len * 2]),
        flags: u32_at(key, 0x38),
        size: u64_at(key, 0x30),
        modified: u64_at(key, 0x10),
    })
}

/// The names in index entries: the file name key of each, up to the last
/// entry.
fn parse_entries(b: &[u8], out: &mut Vec<Name>) {
    let mut off = 0;
    while off + 16 <= b.len() {
        let len = u16_at(b, off + 8) as usize;
        let key_len = u16_at(b, off + 10) as usize;
        let flags = u16_at(b, off + 12);
        if flags & 2 != 0 || len < 16 || off + len > b.len() {
            break;
        }
        let record = u64_at(b, off) & 0xffff_ffff_ffff;
        if key_len > 0 && off + 16 + key_len <= b.len() {
            if let Some(n) = parse_name(record, &b[off + 16..off + 16 + key_len]) {
                out.push(n);
            }
        }
        off += len;
    }
}

/// NTFS time (100 ns steps since 1601) as year, month, day, hour, minute.
fn decode_time(t: u64) -> (u16, u8, u8, u8, u8) {
    const EPOCH_1970: u64 = 11_644_473_600;
    let secs = (t / 10_000_000).saturating_sub(EPOCH_1970);
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    (
        y.clamp(0, 9999) as u16,
        m as u8,
        d as u8,
        (rem / 3600) as u8,
        (rem / 60 % 60) as u8,
    )
}

/// Decompress LZNT1, NTFS's compression: 4 KiB chunks, each a two-byte
/// header and then either plain bytes or tagged groups of literals and
/// back references.
fn lznt1(src: &[u8], out: &mut Vec<u8>, limit: usize) {
    let mut i = 0;
    while i + 2 <= src.len() && out.len() < limit {
        let h = u16_at(src, i);
        if h == 0 {
            break;
        }
        i += 2;
        let size = (h & 0x0fff) as usize + 1;
        let end = (i + size).min(src.len());
        let start = out.len();
        if h & 0x8000 == 0 {
            out.extend_from_slice(&src[i..end]);
            i = end;
            continue;
        }
        while i < end {
            let tags = src[i];
            i += 1;
            for bit in 0..8 {
                if i >= end {
                    break;
                }
                if tags >> bit & 1 == 0 {
                    out.push(src[i]);
                    i += 1;
                    continue;
                }
                if i + 2 > end {
                    i = end;
                    break;
                }
                let t = u16_at(src, i) as usize;
                i += 2;
                let pos = out.len() - start;
                if pos == 0 {
                    return; // a reference before any data: corrupt
                }
                let mut shift = 0;
                let mut p = pos - 1;
                while p >= 0x10 {
                    p >>= 1;
                    shift += 1;
                }
                let back = (t >> (12 - shift)) + 1;
                let len = (t & (0x0fff >> shift)) + 3;
                if back > pos {
                    return;
                }
                for _ in 0..len {
                    let b = out[out.len() - back];
                    out.push(b);
                }
            }
        }
        // a chunk stands for 4 KiB even when it holds less
        let got = out.len() - start;
        if got < 4096 && i < src.len() && u16_at(src, i.min(src.len() - 2)) != 0 {
            out.resize(start + 4096, 0);
        }
    }
}

pub struct Volume {
    disk: Ata,
    /// Where the partition starts on the disk.
    start: u64,
    cluster: usize,
    record: usize,
    mft: Vec<Run>,
    records: BTreeMap<u64, Vec<u8>>,
    dirs: Vec<(u64, Vec<Name>)>,
    /// Size in bytes.
    pub bytes: u64,
    pub label: String,
}

impl Volume {
    pub fn open(disk: Ata, start: u64) -> Result<Volume, Error> {
        let mut v = Volume {
            disk,
            start,
            cluster: 0,
            record: 0,
            mft: Vec::new(),
            records: BTreeMap::new(),
            dirs: Vec::new(),
            bytes: 0,
            label: String::new(),
        };
        let mut b = [0u8; SECTOR];
        v.disk.read(start, &mut b).map_err(|_| Error::Io)?;
        if !is_ntfs(&b) || u16_at(&b, 0x0b) as usize != SECTOR {
            return Err(Error::Unformatted);
        }
        let spc = match b[0x0d] {
            0 => return Err(Error::Unformatted),
            n if n > 0x80 => 1usize << (256 - n as usize).min(20),
            n => n as usize,
        };
        v.cluster = spc * SECTOR;
        // negative: 2^-x bytes; positive: clusters
        let x = b[0x40] as i8;
        v.record = if x < 0 {
            1usize << (-(x as i32)).min(20)
        } else {
            x as usize * spc * SECTOR
        };
        if !(512..=65536).contains(&v.record) {
            return Err(Error::Unformatted);
        }
        v.bytes = u64_at(&b, 0x28) * SECTOR as u64;
        let mft_lcn = u64_at(&b, 0x30);
        // $MFT's own record says where the rest of the MFT is
        let mut rec = vec![0u8; v.record];
        v.disk
            .read(start + mft_lcn * spc as u64, &mut rec)
            .map_err(|_| Error::Io)?;
        if !fixup(&mut rec, b"FILE") {
            return Err(Error::Unformatted);
        }
        match pick(&parse_attrs(&rec), AT_DATA, "").map(|a| a.body) {
            Some(Body::NonResident { runs, .. }) if !runs.is_empty() => v.mft = runs,
            _ => return Err(Error::Unformatted),
        }
        if let Ok(attrs) = v.attrs(VOLUME_RECORD) {
            if let Some(Attr {
                body: Body::Resident(name),
                ..
            }) = pick(&attrs, AT_VOLUME_NAME, "")
            {
                v.label = utf16(&name);
            }
        }
        v.list_record(ROOT_RECORD)?;
        Ok(v)
    }

    /// Read `out.len()` bytes from `offset` in data laid out as `runs`.
    fn read_runs(&mut self, runs: &[Run], offset: u64, out: &mut [u8]) -> Result<(), Error> {
        if out.is_empty() {
            return Ok(());
        }
        let spc = (self.cluster / SECTOR) as u64;
        let first = offset / SECTOR as u64;
        let last = (offset + out.len() as u64).div_ceil(SECTOR as u64);
        let mut buf = vec![0u8; ((last - first) as usize) * SECTOR];
        let mut s = first;
        while s < last {
            let vcn = s / spc;
            let within = s % spc;
            let run = runs
                .iter()
                .find(|r| vcn >= r.vcn && vcn < r.vcn + r.len)
                .copied();
            let at = ((s - first) as usize) * SECTOR;
            let Some(r) = run else {
                // past the runs: nothing was written there
                s += 1;
                buf[at..at + SECTOR].fill(0);
                continue;
            };
            let n = ((r.vcn + r.len - vcn) * spc - within)
                .min(last - s)
                .min(2048);
            let dst = &mut buf[at..at + n as usize * SECTOR];
            match r.lcn {
                None => dst.fill(0),
                Some(lcn) => {
                    let lba = self.start + (lcn + vcn - r.vcn) * spc + within;
                    self.disk.read(lba, dst).map_err(|_| Error::Io)?;
                }
            }
            s += n;
        }
        let skip = (offset - first * SECTOR as u64) as usize;
        out.copy_from_slice(&buf[skip..skip + out.len()]);
        Ok(())
    }

    /// MFT record `n`, fixed up.
    fn record(&mut self, n: u64) -> Result<Vec<u8>, Error> {
        if let Some(r) = self.records.get(&n) {
            return Ok(r.clone());
        }
        let mut rec = vec![0u8; self.record];
        let runs = core::mem::take(&mut self.mft);
        let r = self.read_runs(&runs, n * self.record as u64, &mut rec);
        self.mft = runs;
        r?;
        if !fixup(&mut rec, b"FILE") {
            return Err(Error::NotFound);
        }
        if self.records.len() >= CACHE_RECORDS {
            self.records.clear();
        }
        self.records.insert(n, rec.clone());
        Ok(rec)
    }

    /// All attributes of file `n`, from its base record and the records
    /// its attribute list points to.
    fn attrs(&mut self, n: u64) -> Result<Vec<Attr>, Error> {
        let rec = self.record(n)?;
        if u16_at(&rec, 0x16) & 1 == 0 {
            return Err(Error::NotFound); // a deleted file's record
        }
        let mut attrs = parse_attrs(&rec);
        if let Some(list) = pick(&attrs, AT_ATTRIBUTE_LIST, "") {
            let data = self.data(&list)?;
            let mut more: Vec<u64> = Vec::new();
            let mut off = 0;
            while off + 26 <= data.len() {
                let len = u16_at(&data, off + 4) as usize;
                if len < 26 {
                    break;
                }
                let r = u64_at(&data, off + 16) & 0xffff_ffff_ffff;
                if r != n && !more.contains(&r) {
                    more.push(r);
                }
                off += len;
            }
            for r in more {
                let rec = self.record(r)?;
                attrs.extend(
                    parse_attrs(&rec)
                        .into_iter()
                        .filter(|a| a.kind != AT_ATTRIBUTE_LIST),
                );
            }
        }
        Ok(attrs)
    }

    /// An attribute's whole value.
    fn data(&mut self, a: &Attr) -> Result<Vec<u8>, Error> {
        match &a.body {
            Body::Resident(v) => Ok(v.clone()),
            Body::NonResident {
                runs,
                size,
                initialized,
                unit,
                ..
            } => {
                if a.flags & ENCRYPTED != 0 {
                    return Err(Error::Unsupported);
                }
                if *size > MAX_READ {
                    return Err(Error::TooBig);
                }
                let size = *size as usize;
                if a.flags & COMPRESSED != 0 && *unit != 0 {
                    return self.compressed(runs, size, *unit);
                }
                let mut out = vec![0u8; size];
                let init = (*initialized as usize).min(size);
                self.read_runs(runs, 0, &mut out[..init])?;
                Ok(out)
            }
        }
    }

    fn compressed(&mut self, runs: &[Run], size: usize, unit: u8) -> Result<Vec<u8>, Error> {
        let per = 1u64 << unit.min(8);
        let unit_bytes = per as usize * self.cluster;
        let mut out = Vec::with_capacity(size);
        let mut vcn = 0u64;
        while out.len() < size {
            // how many clusters of this unit are really stored
            let mut stored = 0;
            for c in vcn..vcn + per {
                if runs
                    .iter()
                    .any(|r| c >= r.vcn && c < r.vcn + r.len && r.lcn.is_some())
                {
                    stored += 1;
                }
            }
            let want = (size - out.len()).min(unit_bytes);
            if stored == 0 {
                out.resize(out.len() + want, 0);
            } else {
                let mut raw = vec![0u8; stored as usize * self.cluster];
                self.read_runs(runs, vcn * self.cluster as u64, &mut raw)?;
                if stored == per {
                    out.extend_from_slice(&raw[..want]);
                } else {
                    let start = out.len();
                    lznt1(&raw, &mut out, start + want);
                    out.resize(start + want, 0);
                }
            }
            vcn += per;
        }
        Ok(out)
    }

    /// The names in folder `n`.
    fn list_record(&mut self, n: u64) -> Result<Vec<Name>, Error> {
        if let Some((_, names)) = self.dirs.iter().find(|(r, _)| *r == n) {
            return Ok(names.clone());
        }
        let rec = self.record(n)?;
        if u16_at(&rec, 0x16) & 2 == 0 {
            return Err(Error::NotADirectory);
        }
        let attrs = self.attrs(n)?;
        let mut names = Vec::new();
        let root = match pick(&attrs, AT_INDEX_ROOT, "$I30").map(|a| a.body) {
            Some(Body::Resident(r)) if r.len() >= 32 => r,
            _ => return Err(Error::NotADirectory),
        };
        let block = u32_at(&root, 8) as usize;
        let from = 16 + u32_at(&root, 16) as usize;
        let to = (16 + u32_at(&root, 20) as usize).min(root.len());
        if from < to {
            parse_entries(&root[from..to], &mut names);
        }
        if let Some(alloc) = pick(&attrs, AT_INDEX_ALLOCATION, "$I30") {
            if let Body::NonResident { runs, size, .. } = &alloc.body {
                let bitmap = match pick(&attrs, AT_BITMAP, "$I30") {
                    Some(b) => self.data(&b)?,
                    None => Vec::new(),
                };
                if (512..=65536).contains(&block) {
                    let blocks = (*size / block as u64) as usize;
                    let mut buf = vec![0u8; block];
                    for i in 0..blocks {
                        let used = bitmap.get(i / 8).is_none_or(|b| b >> (i % 8) & 1 != 0);
                        if !used {
                            continue;
                        }
                        self.read_runs(runs, (i * block) as u64, &mut buf)?;
                        if !fixup(&mut buf, b"INDX") {
                            continue;
                        }
                        let from = 0x18 + u32_at(&buf, 0x18) as usize;
                        let to = (0x18 + u32_at(&buf, 0x1c) as usize).min(block);
                        if from < to {
                            parse_entries(&buf[from..to], &mut names);
                        }
                    }
                }
            }
        }
        // one entry per name and file (hard links keep their other names)
        names.sort_by(|a, b| a.record.cmp(&b.record).then_with(|| a.name.cmp(&b.name)));
        names.dedup_by(|a, b| a.record == b.record && a.name == b.name);
        if self.dirs.len() >= CACHE_DIRS {
            self.dirs.remove(0);
        }
        self.dirs.push((n, names.clone()));
        Ok(names)
    }

    /// The record of a path inside the volume ("/" or "" is the root),
    /// and whether it is a folder.
    fn resolve(&mut self, path: &str) -> Result<(u64, bool), Error> {
        let mut cur = ROOT_RECORD;
        let mut dir = true;
        for part in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
            if !dir {
                return Err(Error::NotADirectory);
            }
            let names = self.list_record(cur)?;
            let found = names
                .iter()
                .find(|n| same_name(&n.name, part))
                .ok_or(Error::NotFound)?;
            cur = found.record;
            dir = found.flags & IS_DIR != 0;
        }
        Ok((cur, dir))
    }

    pub fn list(&mut self, path: &str) -> Result<Vec<Info>, Error> {
        let (n, dir) = self.resolve(path)?;
        if !dir {
            return Err(Error::NotADirectory);
        }
        let top = n == ROOT_RECORD;
        Ok(self
            .list_record(n)?
            .into_iter()
            .filter(|e| e.flags & (HIDDEN | SYSTEM) == 0)
            // NTFS's own files ($MFT, $Bitmap, ...) at the top
            .filter(|e| !(top && e.record < 24))
            .map(|e| Info {
                dir: e.flags & IS_DIR != 0,
                size: if e.flags & IS_DIR != 0 { 0 } else { e.size },
                modified: decode_time(e.modified),
                name: e.name,
            })
            .collect())
    }

    pub fn is_dir(&mut self, path: &str) -> bool {
        matches!(self.resolve(path), Ok((_, true)))
    }

    pub fn exists(&mut self, path: &str) -> bool {
        self.resolve(path).is_ok()
    }

    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, Error> {
        let (n, dir) = self.resolve(path)?;
        if dir {
            return Err(Error::IsADirectory);
        }
        let attrs = self.attrs(n)?;
        match pick(&attrs, AT_DATA, "") {
            Some(a) => self.data(&a),
            None => Ok(Vec::new()),
        }
    }
}
