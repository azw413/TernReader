# TRBK Format (Draft)

This is a **draft** format for pre-rendered books. The goal is to keep on-device
logic minimal by doing EPUB parsing, layout, and pagination off-device (desktop/mobile).
Pages are stored as **draw operations**, not full bitmaps.

## Design goals
- Fast page turning, low RAM usage on device.
- Stable binary format with forward-compatible versioning.
- Allows multiple renditions (font size/spacing) generated off-device.
- Supports embedded images (using TRIM payloads).
 - Avoids bitmap page bloat by storing text as draw ops.

## File extension
- `.trbk`

## Endianness
- Little-endian for all multi-byte values.

## Header (fixed + variable)
```
Offset  Size  Field
0x00    4     Magic "TRBK"
0x04    1     Version (u8) = 1
0x05    1     Flags (u8) (reserved)
0x06    2     Header size (u16 LE, bytes)
0x08    2     Screen width  (u16 LE)
0x0A    2     Screen height (u16 LE)
0x0C    4     Page count (u32 LE)
0x10    4     TOC count  (u32 LE)
0x14    4     Page LUT offset (u32 LE)
0x18    4     TOC offset      (u32 LE)
0x1C    4     Page data offset (u32 LE)
0x20    4     Embedded images offset (u32 LE, 0 if none)
0x24    4     Source text hash (u32 LE) (optional)
0x28    4     Reserved

[Variable-length metadata and settings]
```

### Variable metadata block (draft)
All strings are stored as:
```
len (u32 LE) + UTF-8 bytes
```

Suggested fields (in order):
- Title
- Author
- Language
- Identifier
- Font family name
- Font size (u16 LE)
- Line spacing (u16 LE, e.g. 100 = 1.0x)
- Margins (left/right/top/bottom, u16 LE each)

## TOC Table
A list of TOC entries:
```
TOC Entry
- title (string)
- page_index (u32)
- level (u8)
- reserved (3 bytes)
```

## Page LUT
Array of `page_count` entries:
```
page_offset (u32 LE)
```
Offsets are relative to `page_data offset`.

## Page Data
Each page is a sequence of **draw ops** (records). This keeps file sizes small
compared to storing full 480x800 bitmaps (~48KB/page).

```
Record
- opcode (u8)
- length (u16 LE)
- payload (length bytes)
```

### Suggested opcodes
- `0x01` TextRun
  - x (u16), y (u16), style_id (u8), reserved (1 byte)
  - UTF-8 string
- `0x02` Image
  - x (u16), y (u16), width (u16), height (u16)
  - image_id (u32)
- `0x03` LineBreak / Paragraph spacing (optional)

A simple implementation can ignore unknown opcodes.

## Embedded Images
A table of images followed by raw TRIM payloads:
```
Image table:
- image_count (u32)
- for each image:
  - image_id (u32)
  - offset (u32)  // relative to start of embedded images section
  - length (u32)

Image payloads:
- raw TRIM bytes
```

## Notes
- This draft is intentionally simple; we can extend with more opcodes later.
- For initial version, you can skip images and only store text.
- Multiple renditions can be created by outputting multiple `.trbk` files.
