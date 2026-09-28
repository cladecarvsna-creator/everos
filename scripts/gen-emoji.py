#!/usr/bin/env python3
"""Make the emoji pictures for Telegram in EverOS.

Draws every emoji of Noto Color Emoji (and the flags and keycaps, which
are pairs of characters) at 18x18 pixels into one PNG atlas, 64 to a row:
kernel/assets/emoji.png. kernel/assets/emoji.bin lists what each cell
is, as little-endian u32 keys in the order of the cells (sorted):
- a character: its code point;
- a flag: 0x1000_0000 | first << 8 | second (letters 0-25);
- a keycap: 0x2000_0000 | the character (0-9, # or *).

Needs Pillow and fontTools:
    pip install pillow fonttools
    python3 scripts/gen-emoji.py [path to NotoColorEmoji.ttf]
"""
import struct
import sys
from pathlib import Path

from fontTools.ttLib import TTFont
from PIL import Image, ImageDraw, ImageFont

CELL = 18
COLS = 64
FONT = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf"
ROOT = Path(__file__).resolve().parent.parent

font = ImageFont.truetype(FONT, 109)
cmap = TTFont(FONT).getBestCmap()


def draw(text):
    im = Image.new("RGBA", (400, 160), (0, 0, 0, 0))
    ImageDraw.Draw(im).text((0, 0), text, font=font, embedded_color=True)
    box = im.getbbox()
    return im.crop(box) if box else None


def cell(im):
    # fit into the cell, keeping the shape, in the middle
    scale = CELL / max(im.width, im.height)
    w, h = max(1, round(im.width * scale)), max(1, round(im.height * scale))
    small = im.resize((w, h), Image.LANCZOS)
    out = Image.new("RGBA", (CELL, CELL), (0, 0, 0, 0))
    out.paste(small, ((CELL - w) // 2, (CELL - h) // 2))
    return out


pictures = {}
one_wide = None
for cp in sorted(cmap):
    # plain characters, joiners, tags and the flag letters on their own
    if cp < 0x2000 and cp not in (0xA9, 0xAE):
        continue
    if 0x1F1E6 <= cp <= 0x1F1FF or 0xE0000 <= cp <= 0xE007F:
        continue
    if cp in (0x200D, 0x20E3, 0xFE0F, 0x2640, 0x2642) or 0x1F3FB <= cp <= 0x1F3FF:
        if cp not in (0x2640, 0x2642):
            continue
    im = draw(chr(cp))
    if im is None:
        continue
    pictures[cp] = cell(im)
    if cp == 0x1F600:
        one_wide = im.width

# flags: two letters that the font joins into one picture
for a in range(26):
    for b in range(26):
        im = draw(chr(0x1F1E6 + a) + chr(0x1F1E6 + b))
        if im is not None and im.width < one_wide * 1.4:
            pictures[0x1000_0000 | a << 8 | b] = cell(im)

for ch in "0123456789#*":
    im = draw(ch + "️⃣")
    if im is not None and im.width < one_wide * 1.4:
        pictures[0x2000_0000 | ord(ch)] = cell(im)

keys = sorted(pictures)
rows = (len(keys) + COLS - 1) // COLS
atlas = Image.new("RGBA", (COLS * CELL, rows * CELL), (0, 0, 0, 0))
for i, k in enumerate(keys):
    atlas.paste(pictures[k], (i % COLS * CELL, i // COLS * CELL))
assets = ROOT / "kernel" / "assets"
atlas.save(assets / "emoji.png", optimize=True)
(assets / "emoji.bin").write_bytes(b"".join(struct.pack("<I", k) for k in keys))
print(f"{len(keys)} emoji, atlas {atlas.width}x{atlas.height}")
