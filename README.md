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
- SD card `/images` menu with `.tri`/`.trimg` support.
- Portrait UI (480x800) with full-width fit for converted images.
- Selecting an image renders it, then the device sleeps; wake returns to the menu.
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

### TRBK (book format, planned)
We plan to add a simple pre-rendered book format for EPUB conversion.
This keeps firmware fast and low-memory by moving parsing/layout to desktop/mobile.

**Planned structure (draft):**
- **Header**: magic/version, screen size, page count, TOC count, metadata,
  and the font/layout settings used for rendering.
- **TOC table**: entries mapping to page indices.
- **Page LUT**: offsets to page records.
- **Page data**: packed text draw ops + optional embedded images.
- **Embedded images**: stored as TRIM payloads.

The goal is to support multiple renditions (font size/line spacing) generated
off-device, with the device simply paging through pre-rendered content.
