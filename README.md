# TernReader Xteink X4 Rust Firmware with Book and Image viewer

![TernReader logo](ternreader_logo_4color.svg)

This is an alternative firmware for the hugely popular XTEink X4 eReader device. The device is ESP32-C3 based and therefore completely open for hacking and development.

Every other firmware out there for this device is based on PlatformIO and C++, this one is based on embedded Rust and hopefully serves as an example of just how powerful rust is for embedded programming.

## Features

This firmware focuses on two usecases and aims to do each well:- 

* __Wallet for loyalty cards, tickets, boarding passes etc.__ eInk has the great advantage in that the display is persistent and therefore can function even when the device has no power. We convert images into a 4 color greyscale format (trimg) that is compact and renders well on the device.
 
* __eBook Reader__, of course we all love reading on an eInk screen. TernReader converts epub books into a compact binary format (trbk) so that rendering and reading will be fast and small on device.

There is a home screen which shows recents (images & books) by title and thumbnail and also provides access to the file browser to load additional content from the sdcard.

In addition to the firmware image for the device, there are 2 desktop command line tools: `tern-image` and `tern-book`

All of these can be found in the releases section in github.

Additional features:
- Portrait UI (480x800) with a fast Home screen and recents.
- File browser with folders + `.tri`/`.trimg`/`.trbk` entries.
- eBook reader with page indicator, embedded image support, TOC, resume, and sleep overlay.
- Image viewer with previous/next navigation and sleep.
- Auto-sleep after inactivity (5 minutes).


### Image Viewer 
The image viewer views full screen images in 4 color greyscale by selecting the image file in the file browser. Pressing right or left will display the previous or next image in that directory on the sdcard. This is handy, if you put all of your passes in the same directory on the sdcard. Pressing the power button will cause the device to sleep, leaving the image on the screen. The device will sleep in any case after 5 minutes of inactivity.

### eBook Reader
Opening a trbk file in the file browser will open the book for reading. Books retain original epub content including embedded images and ToC which can be used for navigation. Pressing down will advance to the next page, pressing up will go back to previous page. Fonts are rendered antialiased using the font specified at conversion time with `tern-book`.


### Home Screen

### Button guide

| Button | Home | File Browser | Book Reader | Image Viewer | Sleep |
| --- | --- | --- | --- | --- |-------|
| Up | Move selection | Move selection | Previous page | Previous image | -     |
| Down | Move selection | Move selection | Next page | Next image | -     |
| Left | Switch to Actions | — | Previous page | Previous image | -     |
| Right | Switch to Actions | — | Next page | Next image | -     |
| Confirm | Open recent/action | Open | TOC / confirm | — | -     |
| Back | — | Up one folder / Home | Back to Home | Back to Home | -     |
| Power | Sleep | Sleep | Sleep | Sleep | Wake  |



### Command-line tools

The tools are distributed in GitHub Releases for macOS, Linux, and Windows.

**Convert images (tern-image):**
```
# Defaults are already 480x800, fit=width, dither=bayer.
tern-image convert input.png output.tri
```

**Convert images with YOLO barcode/QR detection (recommended for QR/barcodes):**
```
tern-image convert input.png output.tri \
  --yolo-model tools/tern-image/model/YOLOV8s_Barcode_Detection.onnx
```

**Convert books (tern-book):**
```
tern-book input.epub sdcard/MyBook.trbk \
  --font /System/Library/Fonts/Supplemental/Arial.ttf --sizes 24
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

### Installing the firmware
1. Goto https://xteink.dve.al/
2. Backup your existing firmware, by selecting 'Save full flash' under Full Flash Controls
3. Now flash TernReader by selecting the file `ternfull-<VERSION>.bin` from the release under Full Flash Controls
4. Click, 'Write full flash from file'
5. When complete, press the little rest button on the side of the device.

Make sure you have some suitable content on the sdcard. 

___

This repo was originally cloned from: https://github.com/HookedBehemoth/TrustyReader be sure to check back there. Since then book and image viewing have been added here.

## Build
- Rust & cargo
- riscv32 toolchain https://docs.espressif.com/projects/rust/book/getting-started/toolchain.html
- [espflash](https://github.com/esp-rs/espflash/tree/main/espflash/)

Since I want to keep the original partition layout but still use the espflash utils, there is `run.sh` which builds and runs a firmware image.

Can be ran on desktop with `cargo run --package tern-desktop`

To build, flash and run on device use `./run.sh`

## Flashing

There are two firmware images you can flash:

- **Application image** (`firmware.bin` / `tern-fw-<tag>.bin`): contains only the app, meant to be written at `0x10000`.
- **Full merged image** (`ternfull-<tag>.bin`): includes bootloader, partitions, boot_app0, and the app.

Use the **application image** if you already have a working bootloader/partition table.
Use the **full merged image** for a clean flash or if your device is blank.

### Flash app-only (safe update)
```
cargo espflash flash --chip esp32c3 --target riscv32imc-unknown-none-elf \
  --partition-table partition-table.bin \
  --bootloader bootloader.bin \
  --boot-app0 boot_app0.bin \
  --baud 921600 \
  firmware.bin
```

### Flash full merged image (clean flash)
```
./make_full_flash.sh
# then flash ternfull-<tag>.bin with your preferred tool, for example:
cargo espflash flash --chip esp32c3 --target riscv32imc-unknown-none-elf \
  --baud 921600 \
  ternfull-<tag>.bin
```

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

The `tern-image` tool converts PNG/JPG into a mono1 `.tri`/`.trimg` format
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
cargo run -p tern-image -- convert images/Waitrose.PNG images/Waitrose.tri
```

Explicit size/fit/dither:
```
cargo run -p tern-image -- convert input.png output.tri --size 480x800 --fit width --dither bayer
```

Enable debug output:
```
cargo run -p tern-image -- convert input.png output.tri --debug
```

Use YOLOv8 ONNX detector to refine barcode/QR bounding boxes:
```
cargo run -p tern-image -- convert input.png output.tri --debug \
  --yolo-model tools/tern-image/model/YOLOV8s_Barcode_Detection.onnx
```

### Notes
- For ONNX usage, the model must be `.onnx` (not `.pt`/`.safetensors`).
- The ONNX export is fixed to 1x3x640x640 input.

## Book Conversion

The `tern-book` tool converts EPUB into the pre-rendered `.trbk` format.
It runs as a library-first crate with a simple CLI.

### Examples
Basic conversion with a single font and size:
```
cargo run -p tern-book -- input.epub sdcard/MyBook.trbk \
  --font /System/Library/Fonts/Supplemental/Arial.ttf \
  --sizes 18
```

Multiple output sizes in one pass:
```
cargo run -p tern-book -- input.epub sdcard/MyBook.trbk \
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
`tern-image` outputs `.tri`/`.trimg` files. These are identical formats:

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
