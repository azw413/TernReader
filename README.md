# Xteink X4 Rust Firmware with Book and Image viewer

This should eventually turn into a usable firmware for the Xteink X4.

This repo was originally cloned from: https://github.com/HookedBehemoth/TrustyReader be sure to check back there. Since then book and image viewing have been added.

## Build
- Rust & cargo
- riscv32 toolchain https://docs.espressif.com/projects/rust/book/getting-started/toolchain.html
- [espflash](https://github.com/esp-rs/espflash/tree/main/espflash/)

Since I want to keep the original partition layout but still use the espflash utils, there is `run.sh` which builds and runs a firmware image.

Can be ran on desktop with `cargo run --package trusty-desktop`

To build, flash and run on device use `./run.sh`

## Structure
Try to put everything in [Core](/core/), so you can run it on a desktop.

## Firmware status
- Home menu (recents + quick actions).
- SD card file browser with folders and `.tri`/`.trimg`/`.trbk` entries.
- Image viewer runs on desktop and device.
- Book reader: paged layout, TOC, page indicator, resume.
- Portrait UI (480x800) with full-width fit for converted images.
- Barcode/QR re-rendering improves scan reliability.

## Resources
- https://github.com/esp-rs/esp-hal
- https://github.com/sunwoods/Xteink-X4/
- https://github.com/CidVonHighwind/microreader/
- https://www.youtube.com/watch?v=0OMlUCyA_Ys
- https://github.com/HookedBehemoth/microreader/tree/research


## Image Conversion

The `trusty-image` tool converts PNG/JPG into a mono1 `.tri`/`.trimg` format
optimized for the X4 portrait display (480x800). It also detects barcodes/QRs
and re-renders them without dithering for scan reliability.

### Current capabilities
- Defaults to 480x800 portrait output (mono1 bitpacked).
- Aspect-fit modes: contain, cover, stretch, integer, width (default).
- Dithering: Bayer or none.
- Barcode/QR detection (rxing) with crisp overlay re-rendering.
- Optional ONNX detector (YOLOv8) to refine bounding boxes.
- Debug logging for detections, bounding boxes, and overlay placement.

### Examples
Basic conversion (defaults: 480x800, fit=width, dither=bayer):
```
cargo run -p trusty-image -- convert images/Waitrose.PNG images/Waitrose.tri
```

Explicit size/fit/dither:
```
cargo run -p trusty-image -- convert input.png output.tri --size 480x800 --fit width --dither bayer
```

Enable debug output:
```
cargo run -p trusty-image -- convert input.png output.tri --debug
```

Use YOLOv8 ONNX detector to refine barcode/QR bounding boxes:
```
cargo run -p trusty-image -- convert input.png output.tri --debug \
  --yolo-model tools/trusty-image/model/YOLOV8s_Barcode_Detection.onnx
```

### Notes
- For ONNX usage, the model must be `.onnx` (not `.pt`/`.safetensors`).
- The ONNX export is fixed to 1x3x640x640 input.

## Book Conversion

The `trusty-book` tool converts EPUB into the pre-rendered `.trbk` format.
It runs as a library-first crate with a simple CLI.

### Examples
Basic conversion with a single font and size:
```
cargo run -p trusty-book -- input.epub sdcard/MyBook.trbk \
  --font /System/Library/Fonts/Supplemental/Arial.ttf \
  --sizes 18
```

Multiple output sizes in one pass:
```
cargo run -p trusty-book -- input.epub sdcard/MyBook.trbk \
  --font /System/Library/Fonts/Supplemental/Times\ New\ Roman.ttf \
  --sizes 12,16,20
```

### Fonts and styles
- The converter expects a base font (`--font`) in TTF/OTF format.
- If bold/italic text is detected in the book, the converter will look for
  matching font files using common naming conventions:
  - `FontName Bold.ttf`
  - `FontName Italic.ttf`
  - `FontName Bold Italic.ttf`
- If a style is referenced by the book but the matching font file is not found,
  a warning is emitted and the base font is used instead.

## File Formats

### TRIM / TRI (images)
`trusty-image` outputs `.tri`/`.trimg` files. These are identical formats:

```
Offset  Size  Field
0x00    4     Magic "TRIM"
0x04    1     Version (u8) = 1
0x05    1     Format  (u8) = 1 (mono1)
0x06    2     Width   (u16 LE)
0x08    2     Height  (u16 LE)
0x0A    6     Reserved (zeros)
0x10    ...   Bitpacked pixels (row-major, MSB-first)
```

Payload length is `ceil(width * height / 8)`. Total file size is `16 + payload`.

**TRIM v2 (gray2):**
```
Offset  Size  Field
0x00    4     Magic "TRIM"
0x04    1     Version (u8) = 2
0x05    1     Format  (u8) = 2 (gray2)
0x06    2     Width   (u16 LE)
0x08    2     Height  (u16 LE)
0x0A    6     Reserved (zeros)
0x10    ...   Base (BW) bitplane
...     ...   LSB bitplane
...     ...   MSB bitplane
```

Each plane is `ceil(width * height / 8)` bytes. Total payload is `3 * plane`.

### TRBK (book format)
TRBK is a pre-rendered book format generated on desktop. It keeps the firmware
fast and low-memory by moving EPUB parsing/layout off-device.

**Header (v2):**
- Magic/version
- Screen size
- Page count
- TOC count
- Offsets: page LUT, TOC, page data, images, glyph table
- Metadata: title/author/language/identifier/font name
- Layout: char width, line height, ascent, margins

**Tables/blocks:**
- **TOC**: title + page index + level
- **Page LUT**: `u32` offsets into page data
- **Page data**: sequence of draw ops
  - `0x01 TextRun`: x, y, style, utf-8 text
  - `0x02 Image`: x, y, w, h, image index
- **Glyph table**: bitmap glyphs (per style/codepoint)
- **Embedded images**: stored as TRIM payloads with a small image table

The device streams pages from the LUT and renders ops directly.

## Reader & Sleep
### Home Menu
- The device boots into a **Home** menu.
- Top section: **Recents** list (books + images). Each item shows a thumbnail and title.
- Bottom section: **Quick Actions** (File Browser, Settings, Battery).
- Navigation: Up/Down moves through recents, Right/Left switches Quick Actions.

### File Browser
- Starts at SD root on device and `/sdcard` in desktop.
- Supports folders and file filtering.
- `.trbk` opens the book reader, `.tri`/`.trimg` open the image viewer.
- `.epub` entries are shown but prompt for conversion.

### Book Reader
- Paged layout, TOC menu, bottom-right page indicator (current/total).
- Resume state is stored per book (saved on sleep and when exiting to Home).
- Page turns use fast refresh with periodic full refresh to limit ghosting.

### Image Viewer
- Displays `.tri`/`.trimg` in portrait orientation.
- After render the device sleeps; power button returns to Home.
- Barcode/QR regions are re-rendered crisply to improve scan reliability.

### Sleep & Resume
- Inactivity timeout triggers sleep; power button can also force sleep.
- A “Sleeping…” badge is shown before deep sleep.
- Sleep overlay uses current book/image cover as wallpaper where available.
