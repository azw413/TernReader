use std::io::{self, Write};
use std::path::{Path, PathBuf};

use image::{DynamicImage, GrayImage};
use rxing::{
    BarcodeFormat, BinaryBitmap, DecodeHintValue, DecodeHints, Luma8LuminanceSource,
    MultiFormatReader, MultiFormatWriter, Point,
};
use rxing::common::{BitMatrix, HybridBinarizer};
use rxing::multi::{GenericMultipleBarcodeReader, MultipleBarcodeReader};
use rxing::Writer;

mod onnx_detector;

const MAGIC: &[u8; 4] = b"TRIM";
const VERSION_V1: u8 = 1;
const VERSION_V2: u8 = 2;
const FORMAT_MONO1: u8 = 1;
const FORMAT_GRAY2: u8 = 2;

#[derive(Clone, Copy, Debug)]
pub enum FitMode {
    Contain,
    Cover,
    Stretch,
    Integer,
    Width,
}

#[derive(Clone, Copy, Debug)]
pub enum DitherMode {
    Bayer,
    None,
}

#[derive(Clone, Copy, Debug)]
pub enum RegionMode {
    Auto,
    None,
    Crisp,
    Barcode,
}

#[derive(Clone, Debug)]
pub struct ConvertOptions {
    pub width: u32,
    pub height: u32,
    pub fit: FitMode,
    pub dither: DitherMode,
    pub region_mode: RegionMode,
    pub invert: bool,
    pub debug: bool,
    pub yolo_model: Option<PathBuf>,
    pub yolo_num_classes: usize,
    pub yolo_confidence: f32,
    pub yolo_nms: f32,
    pub trimg_version: u8,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            width: 480,
            height: 800,
            fit: FitMode::Width,
            dither: DitherMode::Bayer,
            region_mode: RegionMode::Auto,
            invert: false,
            debug: false,
            yolo_model: None,
            yolo_num_classes: 2,
            yolo_confidence: 0.25,
            yolo_nms: 0.45,
            trimg_version: VERSION_V1,
        }
    }
}

#[derive(Debug)]
pub enum ConvertError {
    Decode,
    Io(io::Error),
}

pub enum TrimgData {
    Mono1(Vec<u8>),
    Gray2 { lsb: Vec<u8>, msb: Vec<u8> },
}

pub struct Trimg {
    pub width: u32,
    pub height: u32,
    pub data: TrimgData,
    pub version: u8,
}

pub fn convert_bytes(bytes: &[u8], options: ConvertOptions) -> Result<Trimg, ConvertError> {
    let image = image::load_from_memory(bytes).map_err(|_| ConvertError::Decode)?;
    Ok(convert_image(&image, options))
}

pub fn convert_image(image: &DynamicImage, options: ConvertOptions) -> Trimg {
    let gray = image.to_luma8();
    let transform = Transform::new(gray.dimensions(), options.width, options.height, options.fit);
    let threshold = otsu_threshold(&gray);
    let (overlays, wipe_rects) = match options.region_mode {
        RegionMode::None => (Vec::new(), Vec::new()),
        RegionMode::Crisp => (Vec::new(), Vec::new()),
        RegionMode::Barcode | RegionMode::Auto => {
            decode_and_render_overlays(image, &gray, &transform, &options)
        }
    };
    let crisp_mask = match options.region_mode {
        RegionMode::None => None,
        RegionMode::Crisp => Some(build_crisp_mask(&gray, threshold, 16)),
        RegionMode::Barcode => None,
        RegionMode::Auto => {
            if overlays.is_empty() {
                Some(build_crisp_mask(&gray, threshold, 16))
            } else {
                None
            }
        }
    };

    if options.trimg_version == VERSION_V1 {
        let mut bits = vec![0u8; ((options.width as usize * options.height as usize) + 7) / 8];
        for y in 0..options.height {
            for x in 0..options.width {
                let mut white = None;
                for overlay in &overlays {
                    if let Some(value) = overlay.sample(x, y) {
                        white = Some(value);
                        break;
                    }
                }

                let mut white = if let Some(value) = white {
                    value
                } else {
                    if wipe_rects.iter().any(|rect| rect.contains(x, y)) {
                        true
                    } else {
                    let (src_x, src_y, in_bounds) = transform.map_to_source(x, y);
                    let lum = if in_bounds {
                        gray.get_pixel(src_x, src_y).0[0]
                    } else {
                        255
                    };
                    if let Some(mask) = &crisp_mask {
                        if in_bounds && mask.is_crisp(src_x, src_y) {
                            lum >= threshold
                        } else {
                            apply_dither(lum, x, y, options.dither)
                        }
                    } else {
                        apply_dither(lum, x, y, options.dither)
                    }
                    }
                };

                if options.invert {
                    white = !white;
                }

                let idx = (y * options.width + x) as usize;
                let byte = idx / 8;
                let bit = 7 - (idx % 8);
                if white {
                    bits[byte] |= 1 << bit;
                }
            }
        }

        Trimg {
            width: options.width,
            height: options.height,
            data: TrimgData::Mono1(bits),
            version: VERSION_V1,
        }
    } else {
        let count = ((options.width as usize * options.height as usize) + 7) / 8;
        let mut lsb = vec![0u8; count];
        let mut msb = vec![0u8; count];
        let bayer: [[i16; 4]; 4] = [
            [0, 8, 2, 10],
            [12, 4, 14, 6],
            [3, 11, 1, 9],
            [15, 7, 13, 5],
        ];
        for y in 0..options.height {
            for x in 0..options.width {
                let mut white = None;
                for overlay in &overlays {
                    if let Some(value) = overlay.sample(x, y) {
                        white = Some(value);
                        break;
                    }
                }

                let mut level = if let Some(value) = white {
                    if value { 0u8 } else { 3u8 }
                } else {
                    if wipe_rects.iter().any(|rect| rect.contains(x, y)) {
                        0u8
                    } else {
                        let (src_x, src_y, in_bounds) = transform.map_to_source(x, y);
                        let lum = if in_bounds {
                            gray.get_pixel(src_x, src_y).0[0]
                        } else {
                            255
                        };
                        let lum = if let Some(mask) = &crisp_mask {
                            if in_bounds && mask.is_crisp(src_x, src_y) {
                                if lum >= threshold { 255 } else { 0 }
                            } else {
                                lum
                            }
                        } else {
                            lum
                        };
                        let dither = match options.dither {
                            DitherMode::Bayer => (bayer[(y as usize) & 3][(x as usize) & 3] - 7) * 4,
                            DitherMode::None => 0,
                        };
                        let adjusted = (lum as i16 + dither).clamp(0, 255) as u8;
                        ((adjusted as u16 * 4) / 256) as u8
                    }
                };

                if options.invert {
                    level = 3 - level;
                }

                let idx = (y * options.width + x) as usize;
                let byte = idx / 8;
                let bit = 7 - (idx % 8);
                if (level & 0x01) != 0 {
                    lsb[byte] |= 1 << bit;
                }
                if (level & 0x02) != 0 {
                    msb[byte] |= 1 << bit;
                }
            }
        }
        Trimg {
            width: options.width,
            height: options.height,
            data: TrimgData::Gray2 { lsb, msb },
            version: VERSION_V2,
        }
    }
}

pub fn write_trimg(path: &Path, trimg: &Trimg) -> io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    let mut header = [0u8; 16];
    header[0..4].copy_from_slice(MAGIC);
    header[4] = trimg.version;
    header[5] = match trimg.data {
        TrimgData::Mono1(_) => FORMAT_MONO1,
        TrimgData::Gray2 { .. } => FORMAT_GRAY2,
    };
    header[6..8].copy_from_slice(&(trimg.width as u16).to_le_bytes());
    header[8..10].copy_from_slice(&(trimg.height as u16).to_le_bytes());
    file.write_all(&header)?;
    match &trimg.data {
        TrimgData::Mono1(bits) => file.write_all(bits)?,
        TrimgData::Gray2 { lsb, msb } => {
            file.write_all(lsb)?;
            file.write_all(msb)?;
        }
    }
    Ok(())
}

pub fn parse_trimg(data: &[u8]) -> Option<Trimg> {
    if data.len() < 16 || &data[0..4] != MAGIC {
        return None;
    }
    let version = data[4];
    let format = data[5];
    let width = u16::from_le_bytes([data[6], data[7]]) as u32;
    let height = u16::from_le_bytes([data[8], data[9]]) as u32;
    let expected = ((width as usize * height as usize) + 7) / 8;
    let payload = &data[16..];
    match (version, format) {
        (VERSION_V1, FORMAT_MONO1) => {
            if payload.len() != expected {
                return None;
            }
            Some(Trimg {
                width,
                height,
                data: TrimgData::Mono1(payload.to_vec()),
                version,
            })
        }
        (VERSION_V2, FORMAT_GRAY2) => {
            if payload.len() != expected * 2 {
                return None;
            }
            let lsb = payload[..expected].to_vec();
            let msb = payload[expected..].to_vec();
            Some(Trimg {
                width,
                height,
                data: TrimgData::Gray2 { lsb, msb },
                version,
            })
        }
        _ => None,
    }
}

struct BarcodeOverlay {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    matrix: BitMatrix,
    scale_x: u32,
    scale_y: u32,
    linear: bool,
}

struct WipeRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl WipeRect {
    fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

impl BarcodeOverlay {
    fn sample(&self, x: u32, y: u32) -> Option<bool> {
        if x < self.x || y < self.y {
            return None;
        }
        let rx = x - self.x;
        let ry = y - self.y;
        if rx >= self.width || ry >= self.height {
            return None;
        }
        let mx = rx / self.scale_x;
        let my = if self.linear { 0 } else { ry / self.scale_y };
        Some(self.matrix.get(mx, my))
    }
}

fn decode_and_render_overlays(
    image: &DynamicImage,
    gray: &GrayImage,
    transform: &Transform,
    options: &ConvertOptions,
) -> (Vec<BarcodeOverlay>, Vec<WipeRect>) {
    let mut yolo_detector = None;
    if let Some(model_path) = &options.yolo_model {
        match onnx_detector::OnnxDetector::load(
            model_path,
            640,
            640,
            options.yolo_num_classes,
            options.yolo_confidence,
            options.yolo_nms,
        ) {
            Ok(detector) => {
                if options.debug {
                    eprintln!(
                        "[trusty-image] onnx model loaded: {} classes",
                        options.yolo_num_classes
                    );
                }
                yolo_detector = Some(detector);
            }
            Err(err) => {
                if options.debug {
                    eprintln!("[trusty-image] onnx model load failed: {err:?}");
                }
            }
        }
    }

    let detections = detect_barcodes(gray, image, options.debug, yolo_detector.as_ref());
    if detections.is_empty() {
        if options.debug {
            eprintln!("[trusty-image] no barcodes detected");
        }
        return (Vec::new(), Vec::new());
    }

    if options.debug {
        eprintln!("[trusty-image] detected {} barcode(s)", detections.len());
        for (i, det) in detections.iter().enumerate() {
            let text_preview = if det.text.len() > 64 {
                format!("{}…", &det.text[..64])
            } else {
                det.text.clone()
            };
            eprintln!(
                "[trusty-image] det[{i}] format={:?} text_len={} text=\"{}\" bbox=({:.1},{:.1})-({:.1},{:.1})",
                det.format,
                det.text.len(),
                text_preview,
                det.rect.min_x,
                det.rect.min_y,
                det.rect.max_x,
                det.rect.max_y
            );
        }
    }

    let mut overlays = Vec::new();
    let mut wipe_rects = Vec::new();
    for detection in detections {
        let is_linear = is_linear_format(&detection.format);
        let mut panel_rect = detection.rect;
        if is_linear {
            if let Some(panel) = find_white_panel(gray, detection.rect, threshold_for_white(gray)) {
                panel_rect = panel;
            }
        }
        let dst_rect = transform.source_rect_to_dest(panel_rect);
        let (mut x, mut y, mut width, mut height) = dst_rect;
        if width < 8 || height < 8 {
            continue;
        }
        let pad = 4;
        expand_rect(
            &mut x,
            &mut y,
            &mut width,
            &mut height,
            transform.dst_w,
            transform.dst_h,
            pad,
        );
        wipe_rects.push(WipeRect { x, y, width, height });

        let writer = MultiFormatWriter::default();
        let base_matrix = match writer.encode(&detection.text, &detection.format, 0, 0) {
            Ok(matrix) => matrix,
            Err(_) => continue,
        };
        let module_w = base_matrix.width();
        let module_h = base_matrix.height();
        if module_w == 0 || module_h == 0 {
            continue;
        }

        let center_x = x + width / 2;
        let center_y = y + height / 2;
        let max_w = width.saturating_add(pad * 2).max(1);
        let max_h = height.saturating_add(pad * 2).max(1);
        let is_linear = is_linear && module_h == 1;

        let base_scale_x = (width / module_w).max(1);
        let max_scale_x = (max_w / module_w).max(1);
        let scale_x = base_scale_x.min(max_scale_x).max(1);

        let (overlay_w, overlay_h, scale_y) = if is_linear {
            let overlay_h = height.saturating_add(pad * 2).max(24).min(max_h).max(1);
            (module_w.saturating_mul(scale_x), overlay_h, overlay_h)
        } else {
            let base_scale = (width / module_w).min(height / module_h).max(1);
            let max_scale = (max_w / module_w).min(max_h / module_h).max(1);
            let scale = base_scale.min(max_scale).max(1);
            (module_w.saturating_mul(scale), module_h.saturating_mul(scale), scale)
        };
        if overlay_w == 0 || overlay_h == 0 {
            continue;
        }

        let mut ox = center_x.saturating_sub(overlay_w / 2);
        let mut oy = center_y.saturating_sub(overlay_h / 2);
        if ox + overlay_w > transform.dst_w {
            ox = transform.dst_w.saturating_sub(overlay_w);
        }
        if oy + overlay_h > transform.dst_h {
            oy = transform.dst_h.saturating_sub(overlay_h);
        }

        // If linear barcode, only allow horizontal growth, keep vertical within original box.
        if is_linear {
            let overlay_h = height.saturating_add(pad * 2).max(24);
            let scale_y = overlay_h;
            oy = y + ((height.saturating_sub(overlay_h)) / 2);
            let min_x = x.saturating_sub(pad);
            let max_x = (x + width + pad)
                .saturating_sub(overlay_w)
                .min(transform.dst_w.saturating_sub(overlay_w));
            if min_x <= max_x {
                ox = ox.clamp(min_x, max_x);
            }
            if options.debug {
                eprintln!(
                    "[trusty-image] linear adjust: bbox_h={} overlay_h={} y={}..{} panel=({:.1},{:.1})-({:.1},{:.1})",
                    height,
                    overlay_h,
                    oy,
                    oy + overlay_h,
                    panel_rect.min_x,
                    panel_rect.min_y,
                    panel_rect.max_x,
                    panel_rect.max_y
                );
            }

            overlays.push(BarcodeOverlay {
                x: ox,
                y: oy,
                width: overlay_w,
                height: overlay_h,
                matrix: base_matrix,
                scale_x,
                scale_y,
                linear: is_linear,
            });
            continue;
        }

        if options.debug {
            eprintln!(
                "[trusty-image] format={:?} text_len={} src_bbox=({:.1},{:.1})-({:.1},{:.1}) dst_rect=({}, {}) {}x{} scale_x={} scale_y={} linear={}",
                detection.format,
                detection.text.len(),
                detection.rect.min_x,
                detection.rect.min_y,
                detection.rect.max_x,
                detection.rect.max_y,
                ox,
                oy,
                overlay_w,
                overlay_h,
                scale_x,
                scale_y,
                is_linear
            );
        }

        overlays.push(BarcodeOverlay {
            x: ox,
            y: oy,
            width: overlay_w,
            height: overlay_h,
            matrix: base_matrix,
            scale_x,
            scale_y,
            linear: is_linear,
        });
    }

    (overlays, wipe_rects)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RectF {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

fn bbox_from_points(points: &[Point]) -> Option<RectF> {
    if points.is_empty() {
        return None;
    }
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for point in points {
        let x = point.x;
        let y = point.y;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    Some(RectF {
        min_x,
        min_y,
        max_x,
        max_y,
    })
}

struct Detection {
    format: BarcodeFormat,
    text: String,
    rect: RectF,
}

fn detect_barcodes(
    gray: &GrayImage,
    image: &DynamicImage,
    debug: bool,
    yolo_detector: Option<&onnx_detector::OnnxDetector>,
) -> Vec<Detection> {
    let formats = [
        BarcodeFormat::QR_CODE,
        BarcodeFormat::CODE_128,
        BarcodeFormat::CODE_39,
        BarcodeFormat::CODE_93,
        BarcodeFormat::EAN_13,
        BarcodeFormat::EAN_8,
        BarcodeFormat::UPC_A,
        BarcodeFormat::UPC_E,
        BarcodeFormat::ITF,
        BarcodeFormat::PDF_417,
        BarcodeFormat::DATA_MATRIX,
    ];
    let mut format_set = std::collections::HashSet::new();
    for fmt in formats {
        format_set.insert(fmt);
    }

    let hints = DecodeHints::default()
        .with(DecodeHintValue::TryHarder(true))
        .with(DecodeHintValue::PossibleFormats(format_set))
        .with(DecodeHintValue::AlsoInverted(true));

    let yolo_detections = if let Some(detector) = yolo_detector {
        match detector.detect(image) {
            Ok(detections) => {
                if debug {
                    eprintln!("[trusty-image] onnx detections: {}", detections.len());
                    for (i, det) in detections.iter().enumerate() {
                        eprintln!(
                            "[trusty-image] onnx[{i}] class={} conf={:.3} bbox=({:.1},{:.1})-({:.1},{:.1})",
                            det.class_index,
                            det.confidence,
                            det.rect.min_x,
                            det.rect.min_y,
                            det.rect.max_x,
                            det.rect.max_y
                        );
                    }
                }
                detections
            }
            Err(err) => {
                if debug {
                    eprintln!("[trusty-image] onnx detection failed: {err:?}");
                }
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let scales = [1.0f32, 0.5, 0.25];
    for &scale in &scales {
        let scaled = if (scale - 1.0).abs() < f32::EPSILON {
            gray.clone()
        } else {
            let w = (gray.width() as f32 * scale).round().max(1.0) as u32;
            let h = (gray.height() as f32 * scale).round().max(1.0) as u32;
            image::imageops::resize(gray, w, h, image::imageops::FilterType::Triangle)
        };

        for invert in [false, true] {
            let detections = decode_with_hints(&scaled, scale, invert, &hints, (0, 0), debug);
            if !detections.is_empty() {
                let mut detections = detections;
                if !yolo_detections.is_empty() {
                    refine_detections_with_yolo(&mut detections, &yolo_detections, debug);
                }
                return detections;
            }
        }
    }

    if !yolo_detections.is_empty() {
        for det in &yolo_detections {
            if let Some(crop_rect) = rect_to_u32(det.rect, gray.width(), gray.height()) {
                let crop = image::imageops::crop_imm(
                    gray,
                    crop_rect.0,
                    crop_rect.1,
                    crop_rect.2,
                    crop_rect.3,
                )
                .to_image();
                for &scale in &scales {
                    let scaled = if (scale - 1.0).abs() < f32::EPSILON {
                        crop.clone()
                    } else {
                        let w = (crop.width() as f32 * scale).round().max(1.0) as u32;
                        let h = (crop.height() as f32 * scale).round().max(1.0) as u32;
                        image::imageops::resize(&crop, w, h, image::imageops::FilterType::Triangle)
                    };
                    for invert in [false, true] {
                        let detections = decode_with_hints(
                            &scaled,
                            scale,
                            invert,
                            &hints,
                            (crop_rect.0, crop_rect.1),
                            debug,
                        );
                        if !detections.is_empty() {
                            let mut detections = detections;
                            for decoded in detections.iter_mut() {
                                decoded.rect = det.rect;
                            }
                            if debug {
                                eprintln!(
                                    "[trusty-image] onnx-assisted decode succeeded for bbox ({:.1},{:.1})-({:.1},{:.1})",
                                    det.rect.min_x,
                                    det.rect.min_y,
                                    det.rect.max_x,
                                    det.rect.max_y
                                );
                            }
                            return detections;
                        }
                    }
                }
            }
        }
    }

    // If full-frame decode fails, try cropping likely barcode bands.
    let bands = find_barcode_bands(gray, debug);
    for band in bands {
        let crop = image::imageops::crop_imm(
            gray,
            band.x,
            band.y,
            band.width,
            band.height,
        )
        .to_image();
        for &scale in &scales {
            let scaled = if (scale - 1.0).abs() < f32::EPSILON {
                crop.clone()
            } else {
                let w = (crop.width() as f32 * scale).round().max(1.0) as u32;
                let h = (crop.height() as f32 * scale).round().max(1.0) as u32;
                image::imageops::resize(&crop, w, h, image::imageops::FilterType::Triangle)
            };
            for invert in [false, true] {
                let detections =
                    decode_with_hints(&scaled, scale, invert, &hints, (band.x, band.y), debug);
                if !detections.is_empty() {
                    let mut detections = detections;
                    if !yolo_detections.is_empty() {
                        refine_detections_with_yolo(&mut detections, &yolo_detections, debug);
                    }
                    return detections;
                }
            }
        }
    }

    Vec::new()
}

fn refine_detections_with_yolo(
    detections: &mut [Detection],
    yolo_detections: &[onnx_detector::OnnxDetection],
    debug: bool,
) {
    for det in detections.iter_mut() {
        if let Some(best) = best_yolo_match(det.rect, yolo_detections) {
            if debug {
                eprintln!(
                    "[trusty-image] onnx refine: ({:.1},{:.1})-({:.1},{:.1}) -> ({:.1},{:.1})-({:.1},{:.1})",
                    det.rect.min_x,
                    det.rect.min_y,
                    det.rect.max_x,
                    det.rect.max_y,
                    best.rect.min_x,
                    best.rect.min_y,
                    best.rect.max_x,
                    best.rect.max_y
                );
            }
            det.rect = best.rect;
        }
    }
}

fn best_yolo_match(
    rect: RectF,
    yolo_detections: &[onnx_detector::OnnxDetection],
) -> Option<onnx_detector::OnnxDetection> {
    let mut best = None;
    let mut best_score = 0.0f32;
    for det in yolo_detections {
        let score = iou_rect(rect, det.rect);
        if score > best_score {
            best_score = score;
            best = Some(det.clone());
        }
    }
    best
}

fn iou_rect(a: RectF, b: RectF) -> f32 {
    let ix_min = a.min_x.max(b.min_x);
    let iy_min = a.min_y.max(b.min_y);
    let ix_max = a.max_x.min(b.max_x);
    let iy_max = a.max_y.min(b.max_y);
    let iw = (ix_max - ix_min).max(0.0);
    let ih = (iy_max - iy_min).max(0.0);
    let intersection = iw * ih;
    let area_a = (a.max_x - a.min_x).max(0.0) * (a.max_y - a.min_y).max(0.0);
    let area_b = (b.max_x - b.min_x).max(0.0) * (b.max_y - b.min_y).max(0.0);
    if area_a <= 0.0 || area_b <= 0.0 {
        return 0.0;
    }
    intersection / (area_a + area_b - intersection)
}

fn rect_to_u32(rect: RectF, max_w: u32, max_h: u32) -> Option<(u32, u32, u32, u32)> {
    let mut x0 = rect.min_x.floor() as i32;
    let mut y0 = rect.min_y.floor() as i32;
    let mut x1 = rect.max_x.ceil() as i32;
    let mut y1 = rect.max_y.ceil() as i32;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    x0 = x0.clamp(0, max_w as i32);
    y0 = y0.clamp(0, max_h as i32);
    x1 = x1.clamp(0, max_w as i32);
    y1 = y1.clamp(0, max_h as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

fn decode_with_hints(
    gray: &GrayImage,
    scale: f32,
    invert: bool,
    hints: &DecodeHints,
    offset: (u32, u32),
    debug: bool,
) -> Vec<Detection> {
    let (src_w, src_h) = gray.dimensions();
    let mut luma = Vec::with_capacity((src_w * src_h) as usize);
    for pixel in gray.pixels() {
        let mut val = pixel.0[0];
        if invert {
            val = 255u8.saturating_sub(val);
        }
        luma.push(val);
    }

    let source = Luma8LuminanceSource::new(luma, src_w, src_h);
    let mut bitmap = BinaryBitmap::new(HybridBinarizer::new(source));
    let reader = MultiFormatReader::default();
    let mut multi = GenericMultipleBarcodeReader::new(reader);
    let results = match multi.decode_multiple_with_hints(&mut bitmap, hints) {
        Ok(results) => results,
        Err(err) => {
            if debug {
                eprintln!(
                    "[trusty-image] decode attempt failed (scale={:.2}, invert={}): {:?}",
                    scale, invert, err
                );
            }
            return Vec::new();
        }
    };

    if debug {
        eprintln!(
            "[trusty-image] decode attempt success (scale={:.2}, invert={}): {} result(s)",
            scale,
            invert,
            results.len()
        );
    }

    let mut detections = Vec::new();
    for result in results {
        let points = result.getPoints();
        let Some(mut rect) = bbox_from_points(points) else { continue };
        if (scale - 1.0).abs() > f32::EPSILON {
            rect.min_x /= scale;
            rect.min_y /= scale;
            rect.max_x /= scale;
            rect.max_y /= scale;
        }
        if is_linear_format(result.getBarcodeFormat()) {
            rect = normalize_linear_rect(rect, src_w as f32, src_h as f32);
            if (rect.max_y - rect.min_y) < 3.0 {
                if let Some(band_rect) = band_rect_for_line(gray, rect.min_y) {
                    rect = band_rect;
                }
            }
        }
        rect.min_x += offset.0 as f32;
        rect.max_x += offset.0 as f32;
        rect.min_y += offset.1 as f32;
        rect.max_y += offset.1 as f32;
        detections.push(Detection {
            format: *result.getBarcodeFormat(),
            text: result.getText().to_string(),
            rect,
        });
    }
    detections
}

fn is_linear_format(format: &BarcodeFormat) -> bool {
    matches!(
        format,
        BarcodeFormat::CODE_128
            | BarcodeFormat::CODE_39
            | BarcodeFormat::CODE_93
            | BarcodeFormat::EAN_13
            | BarcodeFormat::EAN_8
            | BarcodeFormat::UPC_A
            | BarcodeFormat::UPC_E
            | BarcodeFormat::ITF
    )
}

fn normalize_linear_rect(rect: RectF, max_w: f32, max_h: f32) -> RectF {
    let mut rect = rect;
    let width = (rect.max_x - rect.min_x).max(1.0);
    let height = (rect.max_y - rect.min_y).max(1.0);
    if height < width * 0.12 {
        let target_h = (width * 0.35).max(24.0).min(max_h);
        let center_y = rect.min_y;
        let mut min_y = center_y - target_h / 2.0;
        let mut max_y = center_y + target_h / 2.0;
        if min_y < 0.0 {
            max_y -= min_y;
            min_y = 0.0;
        }
        if max_y > max_h {
            let overflow = max_y - max_h;
            min_y = (min_y - overflow).max(0.0);
            max_y = max_h;
        }
        rect.min_y = min_y;
        rect.max_y = max_y;
    }
    rect.min_x = rect.min_x.max(0.0);
    rect.min_y = rect.min_y.max(0.0);
    rect.max_x = rect.max_x.min(max_w);
    rect.max_y = rect.max_y.min(max_h);
    rect
}

fn band_rect_for_line(gray: &GrayImage, y_line: f32) -> Option<RectF> {
    let bands = find_barcode_bands(gray, false);
    if bands.is_empty() {
        return None;
    }
    let mut best = None;
    for band in &bands {
        let y0 = band.y as f32;
        let y1 = (band.y + band.height) as f32;
        if y_line >= y0 && y_line <= y1 {
            best = Some(*band);
            break;
        }
    }
    let band = best.unwrap_or_else(|| bands[0]);
    Some(RectF {
        min_x: band.x as f32,
        min_y: band.y as f32,
        max_x: (band.x + band.width) as f32,
        max_y: (band.y + band.height) as f32,
    })
}

fn threshold_for_white(img: &GrayImage) -> u8 {
    otsu_threshold(img).saturating_add(20).min(240)
}

fn find_white_panel(gray: &GrayImage, rect: RectF, white_threshold: u8) -> Option<RectF> {
    let (w, h) = gray.dimensions();
    let mut min_x = rect.min_x.floor().max(0.0) as i32;
    let mut max_x = rect.max_x.ceil().min(w as f32) as i32;
    let mut min_y = rect.min_y.floor().max(0.0) as i32;
    let mut max_y = rect.max_y.ceil().min(h as f32) as i32;
    if max_x <= min_x || max_y <= min_y {
        return None;
    }

    let white_ratio = |x0: i32, y0: i32, x1: i32, y1: i32| -> f32 {
        let mut white = 0u32;
        let mut total = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let lum = gray.get_pixel(x as u32, y as u32).0[0];
                total += 1;
                if lum >= white_threshold {
                    white += 1;
                }
            }
        }
        white as f32 / total.max(1) as f32
    };

    for _ in 0..32 {
        let mut expanded = false;
        let pad = 8;
        let try_left = (min_x - pad).max(0);
        if try_left < min_x {
            let ratio = white_ratio(try_left, min_y, min_x, max_y);
            if ratio > 0.8 {
                min_x = try_left;
                expanded = true;
            }
        }
        let try_right = (max_x + pad).min(w as i32);
        if try_right > max_x {
            let ratio = white_ratio(max_x, min_y, try_right, max_y);
            if ratio > 0.8 {
                max_x = try_right;
                expanded = true;
            }
        }
        let try_up = (min_y - pad).max(0);
        if try_up < min_y {
            let ratio = white_ratio(min_x, try_up, max_x, min_y);
            if ratio > 0.8 {
                min_y = try_up;
                expanded = true;
            }
        }
        let try_down = (max_y + pad).min(h as i32);
        if try_down > max_y {
            let ratio = white_ratio(min_x, max_y, max_x, try_down);
            if ratio > 0.8 {
                max_y = try_down;
                expanded = true;
            }
        }
        if !expanded {
            break;
        }
    }

    Some(RectF {
        min_x: min_x as f32,
        min_y: min_y as f32,
        max_x: max_x as f32,
        max_y: max_y as f32,
    })
}

#[derive(Clone, Copy)]
struct Band {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    score: f32,
}

fn find_barcode_bands(gray: &GrayImage, debug: bool) -> Vec<Band> {
    let (w, h) = gray.dimensions();
    if w < 32 || h < 32 {
        return Vec::new();
    }

    let mut scores = vec![0f32; h as usize];
    for y in 1..h - 1 {
        let mut sum = 0u32;
        for x in 1..w - 1 {
            let left = gray.get_pixel(x - 1, y).0[0] as i32;
            let right = gray.get_pixel(x + 1, y).0[0] as i32;
            sum += (right - left).abs() as u32;
        }
        scores[y as usize] = sum as f32 / (w.saturating_sub(2).max(1)) as f32;
    }

    let mean = scores.iter().sum::<f32>() / scores.len().max(1) as f32;
    let var = scores
        .iter()
        .map(|v| {
            let d = v - mean;
            d * d
        })
        .sum::<f32>()
        / scores.len().max(1) as f32;
    let stddev = var.sqrt();
    let threshold = mean + stddev * 0.8;

    let mut bands = Vec::new();
    let mut in_band = false;
    let mut start = 0u32;
    let mut acc = 0f32;
    let mut count = 0u32;
    for y in 0..h {
        let score = scores[y as usize];
        if score >= threshold {
            if !in_band {
                in_band = true;
                start = y;
                acc = 0.0;
                count = 0;
            }
            acc += score;
            count += 1;
        } else if in_band {
            let end = y;
            let height = end - start;
            if height >= 24 && height <= (h as f32 * 0.6) as u32 {
                let avg = acc / count.max(1) as f32;
                bands.push(Band {
                    x: 0,
                    y: start,
                    width: w,
                    height,
                    score: avg,
                });
            }
            in_band = false;
        }
    }
    if in_band {
        let end = h;
        let height = end - start;
        if height >= 24 && height <= (h as f32 * 0.6) as u32 {
            let avg = acc / count.max(1) as f32;
            bands.push(Band {
                x: 0,
                y: start,
                width: w,
                height,
                score: avg,
            });
        }
    }

    if bands.is_empty() {
        // Fallback: use strongest rows to seed bands.
        let mut idx: Vec<(usize, f32)> = scores
            .iter()
            .enumerate()
            .map(|(i, v)| (i, *v))
            .collect();
        idx.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (row, score) in idx.into_iter().take(3) {
            let band_h = (h as f32 * 0.25).round() as u32;
            let y0 = row.saturating_sub(band_h as usize / 2) as u32;
            let y1 = (y0 + band_h).min(h);
            bands.push(Band {
                x: 0,
                y: y0,
                width: w,
                height: y1 - y0,
                score,
            });
        }
    }

    // Pad bands to include quiet zones.
    for band in &mut bands {
        let pad_y = (band.height as f32 * 0.2).round() as u32;
        let y0 = band.y.saturating_sub(pad_y);
        let y1 = (band.y + band.height + pad_y).min(h);
        band.y = y0;
        band.height = y1 - y0;
    }

    bands.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    if debug {
        for (i, band) in bands.iter().take(5).enumerate() {
            eprintln!(
                "[trusty-image] band[{i}] y={} h={} score={:.1}",
                band.y, band.height, band.score
            );
        }
    }

    bands.into_iter().take(5).collect()
}

fn expand_rect(x: &mut u32, y: &mut u32, w: &mut u32, h: &mut u32, max_w: u32, max_h: u32, pad: u32) {
    let left = x.saturating_sub(pad);
    let top = y.saturating_sub(pad);
    let right = (*x + *w + pad).min(max_w);
    let bottom = (*y + *h + pad).min(max_h);
    if right <= left + 1 || bottom <= top + 1 {
        return;
    }
    *x = left;
    *y = top;
    *w = right - left;
    *h = bottom - top;
}

fn apply_dither(lum: u8, x: u32, y: u32, mode: DitherMode) -> bool {
    match mode {
        DitherMode::None => lum >= 128,
        DitherMode::Bayer => {
            let bayer: [[u8; 4]; 4] = [
                [0, 8, 2, 10],
                [12, 4, 14, 6],
                [3, 11, 1, 9],
                [15, 7, 13, 5],
            ];
            let threshold = bayer[(y as usize) & 3][(x as usize) & 3] * 16 + 8;
            lum >= threshold
        }
    }
}

fn otsu_threshold(img: &GrayImage) -> u8 {
    let mut hist = [0u32; 256];
    for pixel in img.pixels() {
        hist[pixel.0[0] as usize] += 1;
    }
    let total = img.width() as u64 * img.height() as u64;
    let mut sum_total = 0u64;
    for (i, &count) in hist.iter().enumerate() {
        sum_total += (i as u64) * (count as u64);
    }

    let mut sum_b = 0u64;
    let mut w_b = 0u64;
    let mut max_var = 0f64;
    let mut threshold = 128u8;

    for (i, &count) in hist.iter().enumerate() {
        w_b += count as u64;
        if w_b == 0 {
            continue;
        }
        let w_f = total - w_b;
        if w_f == 0 {
            break;
        }
        sum_b += (i as u64) * (count as u64);
        let m_b = sum_b as f64 / w_b as f64;
        let m_f = (sum_total - sum_b) as f64 / w_f as f64;
        let var_between = (w_b as f64) * (w_f as f64) * (m_b - m_f).powi(2);
        if var_between > max_var {
            max_var = var_between;
            threshold = i as u8;
        }
    }
    threshold
}

struct Transform {
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    scale_x: f32,
    scale_y: f32,
    offset_x: f32,
    offset_y: f32,
    in_bounds_min_x: u32,
    in_bounds_min_y: u32,
    in_bounds_max_x: u32,
    in_bounds_max_y: u32,
}

impl Transform {
    fn new(src: (u32, u32), dst_w: u32, dst_h: u32, fit: FitMode) -> Self {
        let (src_w, src_h) = src;
        let mut scale_x = dst_w as f32 / src_w as f32;
        let mut scale_y = dst_h as f32 / src_h as f32;
        let mut offset_x = 0f32;
        let mut offset_y = 0f32;

        match fit {
            FitMode::Stretch => {}
            FitMode::Contain => {
                let scale = scale_x.min(scale_y);
                scale_x = scale;
                scale_y = scale;
                let new_w = (src_w as f32 * scale).round();
                let new_h = (src_h as f32 * scale).round();
                offset_x = ((dst_w as f32 - new_w) / 2.0).round();
                offset_y = ((dst_h as f32 - new_h) / 2.0).round();
            }
            FitMode::Cover => {
                let scale = scale_x.max(scale_y);
                scale_x = scale;
                scale_y = scale;
                let new_w = (src_w as f32 * scale).round();
                let new_h = (src_h as f32 * scale).round();
                offset_x = ((dst_w as f32 - new_w) / 2.0).round();
                offset_y = ((dst_h as f32 - new_h) / 2.0).round();
            }
            FitMode::Integer => {
                let scale = (dst_w / src_w).min(dst_h / src_h).max(1) as f32;
                scale_x = scale;
                scale_y = scale;
                let new_w = (src_w as f32 * scale).round();
                let new_h = (src_h as f32 * scale).round();
                offset_x = ((dst_w as f32 - new_w) / 2.0).round();
                offset_y = ((dst_h as f32 - new_h) / 2.0).round();
            }
            FitMode::Width => {
                let scale = scale_x;
                scale_x = scale;
                scale_y = scale;
                let new_h = (src_h as f32 * scale).round();
                offset_x = 0.0;
                offset_y = ((dst_h as f32 - new_h) / 2.0).round();
            }
        }

        let min_x = offset_x.max(0.0) as u32;
        let min_y = offset_y.max(0.0) as u32;
        let max_x = (offset_x + (src_w as f32 * scale_x)).min(dst_w as f32) as u32;
        let max_y = (offset_y + (src_h as f32 * scale_y)).min(dst_h as f32) as u32;

        Self {
            src_w,
            src_h,
            dst_w,
            dst_h,
            scale_x,
            scale_y,
            offset_x,
            offset_y,
            in_bounds_min_x: min_x,
            in_bounds_min_y: min_y,
            in_bounds_max_x: max_x,
            in_bounds_max_y: max_y,
        }
    }

    fn map_to_source(&self, x: u32, y: u32) -> (u32, u32, bool) {
        if x < self.in_bounds_min_x
            || y < self.in_bounds_min_y
            || x >= self.in_bounds_max_x
            || y >= self.in_bounds_max_y
        {
            return (0, 0, false);
        }
        let src_x = ((x as f32 - self.offset_x) / self.scale_x).floor() as i32;
        let src_y = ((y as f32 - self.offset_y) / self.scale_y).floor() as i32;
        if src_x < 0 || src_y < 0 || src_x >= self.src_w as i32 || src_y >= self.src_h as i32 {
            (0, 0, false)
        } else {
            (src_x as u32, src_y as u32, true)
        }
    }

    fn source_rect_to_dest(&self, rect: RectF) -> (u32, u32, u32, u32) {
        let x0 = (rect.min_x as f32 * self.scale_x + self.offset_x).floor();
        let y0 = (rect.min_y as f32 * self.scale_y + self.offset_y).floor();
        let x1 = (rect.max_x as f32 * self.scale_x + self.offset_x).ceil();
        let y1 = (rect.max_y as f32 * self.scale_y + self.offset_y).ceil();

        let x0 = x0.max(0.0) as u32;
        let y0 = y0.max(0.0) as u32;
        let mut x1 = x1.min(self.dst_w as f32) as u32;
        let mut y1 = y1.min(self.dst_h as f32) as u32;
        if x1 <= x0 + 1 {
            x1 = (x0 + 2).min(self.dst_w);
        }
        if y1 <= y0 + 1 {
            y1 = (y0 + 2).min(self.dst_h);
        }
        (x0, y0, x1 - x0, y1 - y0)
    }
}

struct CrispMask {
    block_size: u32,
    blocks_x: u32,
    blocks_y: u32,
    mask: Vec<bool>,
}

impl CrispMask {
    fn is_crisp(&self, x: u32, y: u32) -> bool {
        let bx = (x / self.block_size).min(self.blocks_x - 1);
        let by = (y / self.block_size).min(self.blocks_y - 1);
        let idx = (by * self.blocks_x + bx) as usize;
        self.mask.get(idx).copied().unwrap_or(false)
    }
}

fn build_crisp_mask(img: &GrayImage, threshold: u8, block_size: u32) -> CrispMask {
    let (w, h) = img.dimensions();
    let blocks_x = (w + block_size - 1) / block_size;
    let blocks_y = (h + block_size - 1) / block_size;
    let mut mask = vec![false; (blocks_x * blocks_y) as usize];

    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            let x0 = bx * block_size;
            let y0 = by * block_size;
            let x1 = (x0 + block_size).min(w);
            let y1 = (y0 + block_size).min(h);

            let mut black = 0u32;
            let mut white = 0u32;
            let mut transitions = 0u32;
            let mut total = 0u32;

            for y in y0..y1 {
                let mut prev = None;
                for x in x0..x1 {
                    let is_white = img.get_pixel(x, y).0[0] >= threshold;
                    total += 1;
                    if is_white {
                        white += 1;
                    } else {
                        black += 1;
                    }
                    if let Some(prev_val) = prev {
                        if prev_val != is_white {
                            transitions += 1;
                        }
                    }
                    prev = Some(is_white);
                }
            }

            let black_ratio = black as f32 / total.max(1) as f32;
            let white_ratio = white as f32 / total.max(1) as f32;
            let edge_ratio = transitions as f32 / total.max(1) as f32;

            let is_crisp = black_ratio > 0.2 && white_ratio > 0.2 && edge_ratio > 0.15;
            mask[(by * blocks_x + bx) as usize] = is_crisp;
        }
    }

    CrispMask {
        block_size,
        blocks_x,
        blocks_y,
        mask,
    }
}
