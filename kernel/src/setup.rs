//! Installing EverOS on a hard disk, the work behind the installer's
//! screens (gui/setup.rs).
//!
//! The installation disc's GRUB loads three files next to the kernel:
//! the kernel itself, GRUB's boot sector (boot.img) and GRUB's core
//! (core.img, built by the Makefile with the modules it needs to read
//! FAT32 and start a multiboot2 kernel). Installing is what grub-install
//! does on a BIOS computer:
//!
//! 1. The disk gets one FAT32 partition starting at 1 MiB (or keeps the
//!    one it has, with the files on it).
//! 2. The kernel goes to `/boot/kernel.bin` and a grub.cfg next to it.
//! 3. core.img goes into the free sectors between the partition table and
//!    the partition, and boot.img's code into the partition table's
//!    sector, which the BIOS runs. It loads core.img, which reads
//!    grub.cfg from the partition and starts the kernel.
//! 4. The user's account goes to `/EverOS/users.cfg`.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::fs::{self, ata::Ata, Probe};
use crate::multiboot::BootInfo;
use crate::{rtc, serial, users};

const SECTOR: usize = 512;
/// Smallest disk EverOS goes on.
pub const MIN_BYTES: u64 = 64 * 1024 * 1024;
/// The part of boot.img that is code; the partition table comes after.
const BOOT_CODE: usize = 440;
/// How much of the kernel is written per frame.
const STEP_BYTES: usize = 256 * 1024;

const GRUB_CFG: &str = "# Written by the EverOS installer.\n\
set timeout=0\n\
set default=0\n\
insmod all_video\n\
\n\
menuentry \"EverOS\" {\n\
    multiboot2 /boot/kernel.bin\n\
    boot\n\
}\n";

/// The files the disc brought along.
#[derive(Clone, Copy)]
pub struct Files {
    kernel: &'static [u8],
    boot: &'static [u8],
    core: &'static [u8],
}

impl Files {
    pub fn find(boot: &BootInfo) -> Option<Files> {
        let files = Files {
            kernel: boot.module("kernel.bin")?,
            boot: boot.module("boot.img")?,
            core: boot.module("core.img")?,
        };
        (files.boot.len() == SECTOR && !files.kernel.is_empty() && !files.core.is_empty())
            .then_some(files)
    }

    /// Room the system takes on the disk.
    pub fn bytes(&self) -> u64 {
        (self.kernel.len() + self.core.len()) as u64
    }
}

/// A disk the installer can use.
pub struct Disk {
    pub ata: Ata,
    pub model: String,
    pub bytes: u64,
    pub probe: Probe,
}

impl Disk {
    /// Whether EverOS can go on it without erasing the files: it has a
    /// FAT32 partition with room for GRUB's core before it.
    pub fn can_keep(&self, files: &Files) -> bool {
        let core = files.core.len().div_ceil(SECTOR) as u64;
        self.probe.start.is_some_and(|s| s > core) && self.probe.free > files.bytes() + 1024 * 1024
    }

    pub fn big_enough(&self) -> bool {
        self.bytes >= MIN_BYTES
    }
}

/// The hard disks, with what is on them.
pub fn disks() -> Vec<Disk> {
    Ata::all()
        .into_iter()
        .map(|ata| {
            let probe = fs::probe(&ata);
            Disk {
                model: String::from(ata.model()),
                bytes: ata.sectors() * SECTOR as u64,
                probe,
                ata,
            }
        })
        .collect()
}

/// The steps the installing screen lists.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Prepare,
    Copy,
    Bootloader,
    Account,
    Finish,
    Done,
}

pub const STEPS: [Step; 5] = [
    Step::Prepare,
    Step::Copy,
    Step::Bootloader,
    Step::Account,
    Step::Finish,
];

pub struct Install {
    files: Files,
    disk: Ata,
    /// Keep the files on the disk instead of erasing it.
    keep: bool,
    name: String,
    password: String,
    pub step: Step,
    upload: Option<fs::Upload>,
    /// Where the partition starts.
    start: u64,
}

impl Install {
    pub fn new(files: Files, disk: Ata, keep: bool, name: &str, password: &str) -> Self {
        serial::write_str(if keep {
            "setup: installing, keeping the files on the disk\n"
        } else {
            "setup: installing on an erased disk\n"
        });
        Self {
            files,
            disk,
            keep,
            name: String::from(name),
            password: String::from(password),
            step: Step::Prepare,
            upload: None,
            start: 0,
        }
    }

    /// How far the current step is, from 0 to 1000.
    pub fn step_permille(&self) -> u32 {
        match (self.step, &self.upload) {
            (Step::Copy, Some(up)) => up.permille(),
            _ => 0,
        }
    }

    /// Do the next piece of work. Returns an error message for the user
    /// if it failed.
    pub fn work(&mut self) -> Result<(), &'static str> {
        let result = match self.step {
            Step::Prepare => self.prepare(),
            Step::Copy => self.copy(),
            Step::Bootloader => self.bootloader(),
            Step::Account => self.account(),
            Step::Finish => self.finish(),
            Step::Done => Ok(()),
        };
        if let Err(msg) = result {
            serial::write_str("setup: failed: ");
            serial::write_str(msg);
            serial::write_str("\n");
        }
        result
    }

    fn next(&mut self, step: Step, log: &str) {
        serial::write_str(log);
        self.step = step;
    }

    fn prepare(&mut self) -> Result<(), &'static str> {
        const DISK: &str = "The disk could not be written.";
        if self.keep {
            if !fs::mounted_on(&self.disk) {
                fs::unmount();
                fs::mount(self.disk.clone()).map_err(|e| e.message())?;
            }
        } else {
            fs::unmount();
            // wipe what was before the partition, old boot code included
            let zeros = vec![0u8; 128 * SECTOR];
            let mut disk = self.disk.clone();
            let mut lba = 0;
            while lba < 2048 {
                disk.write(lba, &zeros).map_err(|_| DISK)?;
                lba += 128;
            }
            fs::format(self.disk.clone()).map_err(|e| e.message())?;
            fs::mount(self.disk.clone()).map_err(|e| e.message())?;
        }
        self.start = fs::probe(&self.disk).start.ok_or(DISK)?;
        for dir in ["/boot", "/boot/grub", "/EverOS"] {
            if !fs::is_dir(dir) {
                fs::create_dir(dir).map_err(|e| e.message())?;
            }
        }
        let up = fs::begin_upload("/boot/kernel.bin", self.files.kernel.len())
            .map_err(|e| e.message())?;
        self.upload = Some(up);
        self.next(Step::Copy, "setup: copying files\n");
        Ok(())
    }

    fn copy(&mut self) -> Result<(), &'static str> {
        let up = self.upload.as_mut().ok_or("The copy did not start.")?;
        if !fs::upload_step(up, self.files.kernel, STEP_BYTES).map_err(|e| e.message())? {
            return Ok(());
        }
        self.upload = None;
        fs::write("/boot/grub/grub.cfg", GRUB_CFG.as_bytes()).map_err(|e| e.message())?;
        // check the copy against the disc
        let copied = fs::read("/boot/kernel.bin").map_err(|e| e.message())?;
        if copied != self.files.kernel {
            return Err("The copied system is damaged: the disk returned other data.");
        }
        self.next(Step::Bootloader, "setup: installing the boot loader\n");
        Ok(())
    }

    fn bootloader(&mut self) -> Result<(), &'static str> {
        const DISK: &str = "The boot loader could not be written.";
        let sectors = self.files.core.len().div_ceil(SECTOR);
        if 1 + sectors as u64 > self.start {
            return Err("There is no room for the boot loader before the partition.");
        }
        let mut disk = self.disk.clone();
        let mut core = vec![0u8; sectors * SECTOR];
        core[..self.files.core.len()].copy_from_slice(self.files.core);
        disk.write(1, &core).map_err(|_| DISK)?;

        // GRUB's code in the first sector; the partition table stays
        let mut mbr = [0u8; SECTOR];
        disk.read(0, &mut mbr).map_err(|_| DISK)?;
        mbr[..BOOT_CODE].copy_from_slice(&self.files.boot[..BOOT_CODE]);
        for i in 0..4 {
            let entry = &mut mbr[446 + i * 16..462 + i * 16];
            let first = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
            // the BIOS boots the partition marked active
            entry[0] = if first as u64 == self.start && entry[4] != 0 {
                0x80
            } else {
                0
            };
        }
        mbr[510] = 0x55;
        mbr[511] = 0xaa;
        disk.write(0, &mbr).map_err(|_| DISK)?;
        disk.flush().map_err(|_| DISK)?;
        self.next(Step::Account, "setup: creating the account\n");
        Ok(())
    }

    fn account(&mut self) -> Result<(), &'static str> {
        // accounts an earlier installation saved stay; the built-in root
        // of a disk that was only used from the disc does not
        let keep_accounts = self.keep && users::saved();
        users::install_account(&self.name, &self.password, keep_accounts)
            .map_err(|e| e.message())?;
        fs::ensure_home(&self.name);
        self.next(Step::Finish, "setup: finishing\n");
        Ok(())
    }

    fn finish(&mut self) -> Result<(), &'static str> {
        let (year, month, day) = rtc::date();
        let (h, m, _) = rtc::time();
        let text = format!(
            "EverOS {}\r\ninstalled={:04}-{:02}-{:02} {:02}:{:02}\r\nuser={}\r\n",
            env!("CARGO_PKG_VERSION"),
            year,
            month,
            day,
            h,
            m,
            self.name
        );
        fs::write(fs::INSTALLED, text.as_bytes()).map_err(|e| e.message())?;
        self.password.clear();
        self.next(Step::Done, "setup: done\n");
        Ok(())
    }
}
