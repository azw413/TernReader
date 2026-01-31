# Xteink X4 sample rust

This should eventually turn into a usable firmware for the Xteink X4.

## Build
- Rust & cargo
- riscv32 toolchain https://docs.espressif.com/projects/rust/book/getting-started/toolchain.html
- [espflash](https://github.com/esp-rs/espflash/tree/main/espflash/)

Since I want to keep the original partition layout but still use the espflash utils, there is `run.sh` which builds and runs a firmware image.

Can be ran on desktop with `cargo run --package trusty-desktop`

## Structure
Try to put everything in [Core](/core/), so you can run it on a desktop.

## Firmware status
- Image viewer runs on desktop and device.
- SD card file browser with folders and `.tri`/`.trimg`/`.trbk` entries.
- Portrait UI (480x800) with full-width fit for converted images.
- Selecting an image renders it, then the device sleeps; wake returns to the menu.
- Barcode/QR re-rendering improves scan reliability.
- Book reader: paged layout, TOC menu, page indicator, resume.

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

## File Formats

### TRIM / TRI (mono images)
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
- Book reader supports paging, TOC, and a bottom-right page indicator.
- Resume state is written on sleep and restored on wake.
- Inactivity timeout triggers sleep; power button can also force sleep.
- A “Sleeping…” badge is shown before deep sleep.

## File Browser
- Starts at SD root on device and `/sdcard` in desktop.
- Supports folders and file filtering.
- `.trbk` opens the book reader, `.tri`/`.trimg` open the image viewer.
- `.epub` entries are shown but prompt for conversion.

## Greyscale Experiments
Grayscale LUT experiments and TRIM v2 rendering tests live on the
`greyscale-support` branch. Main remains mono for reliability.
