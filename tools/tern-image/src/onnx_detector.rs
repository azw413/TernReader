use std::path::Path;

use image::DynamicImage;
use tract_onnx::prelude::{self, *};

use crate::RectF;

#[derive(Clone, Debug)]
pub struct OnnxDetection {
    pub rect: RectF,
    pub class_index: usize,
    pub confidence: f32,
}

pub struct OnnxDetector {
    model: SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>,
    input_w: usize,
    input_h: usize,
    confidence_threshold: f32,
    nms_threshold: f32,
    num_classes: usize,
}

impl OnnxDetector {
    pub fn load(
        model_path: &Path,
        input_w: usize,
        input_h: usize,
        num_classes: usize,
        confidence_threshold: f32,
        nms_threshold: f32,
    ) -> anyhow::Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_path(model_path)?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(1, 3, input_h, input_w)),
            )?
            .into_optimized()?
            .into_runnable()?;

        Ok(Self {
            model,
            input_w,
            input_h,
            confidence_threshold,
            nms_threshold,
            num_classes,
        })
    }

    pub fn detect(&self, image: &DynamicImage) -> anyhow::Result<Vec<OnnxDetection>> {
        let (resized, scale, pad_x, pad_y) = letterbox(image, self.input_w, self.input_h);
        let tensor = image_to_tensor(&resized)?;
        let outputs = self.model.run(tvec!(tensor.into()))?;
        let output = outputs[0].to_array_view::<f32>()?;
        let shape = output.shape();

        let mut boxes = Vec::new();
        if shape.len() == 3 {
            if shape[1] == 6 {
                let n = shape[2];
                for i in 0..n {
                    let x = output[[0, 0, i]];
                    let y = output[[0, 1, i]];
                    let w = output[[0, 2, i]];
                    let h = output[[0, 3, i]];
                    let (class_index, confidence) =
                        best_class(&output, 0, i, self.num_classes, Layout::FeaturesFirst);
                    if confidence < self.confidence_threshold {
                        continue;
                    }
                    if let Some(rect) = restore_rect(x, y, w, h, scale, pad_x, pad_y, image.width() as f32, image.height() as f32) {
                        boxes.push(OnnxDetection { rect, class_index, confidence });
                    }
                }
            } else if shape[2] == 6 {
                let n = shape[1];
                for i in 0..n {
                    let x = output[[0, i, 0]];
                    let y = output[[0, i, 1]];
                    let w = output[[0, i, 2]];
                    let h = output[[0, i, 3]];
                    let (class_index, confidence) =
                        best_class(&output, 0, i, self.num_classes, Layout::PredictionsFirst);
                    if confidence < self.confidence_threshold {
                        continue;
                    }
                    if let Some(rect) = restore_rect(x, y, w, h, scale, pad_x, pad_y, image.width() as f32, image.height() as f32) {
                        boxes.push(OnnxDetection { rect, class_index, confidence });
                    }
                }
            }
        }

        Ok(nms(boxes, self.nms_threshold))
    }
}

enum Layout {
    FeaturesFirst,
    PredictionsFirst,
}

fn best_class(
    output: &prelude::tract_ndarray::ArrayViewD<'_, f32>,
    b: usize,
    pred_index: usize,
    num_classes: usize,
    layout: Layout,
) -> (usize, f32) {
    let mut best_index = 0;
    let mut best_score = f32::MIN;
    for i in 0..num_classes {
        let score = match layout {
            Layout::FeaturesFirst => output[[b, 4 + i, pred_index]],
            Layout::PredictionsFirst => output[[b, pred_index, 4 + i]],
        };
        if score > best_score {
            best_score = score;
            best_index = i;
        }
    }
    (best_index, best_score)
}

fn restore_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    max_w: f32,
    max_h: f32,
) -> Option<RectF> {
    let x0 = (x - w / 2.0 - pad_x) / scale;
    let y0 = (y - h / 2.0 - pad_y) / scale;
    let x1 = (x + w / 2.0 - pad_x) / scale;
    let y1 = (y + h / 2.0 - pad_y) / scale;
    let min_x = x0.clamp(0.0, max_w);
    let min_y = y0.clamp(0.0, max_h);
    let max_x = x1.clamp(0.0, max_w);
    let max_y = y1.clamp(0.0, max_h);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    Some(RectF { min_x, min_y, max_x, max_y })
}

fn letterbox(image: &DynamicImage, target_w: usize, target_h: usize) -> (DynamicImage, f32, f32, f32) {
    let (w, h) = (image.width() as f32, image.height() as f32);
    let scale = (target_w as f32 / w).min(target_h as f32 / h);
    let new_w = (w * scale).round().max(1.0) as u32;
    let new_h = (h * scale).round().max(1.0) as u32;
    let resized = image.resize_exact(new_w, new_h, image::imageops::FilterType::CatmullRom);
    let mut canvas = image::ImageBuffer::from_pixel(
        target_w as u32,
        target_h as u32,
        image::Rgb([114u8, 114u8, 114u8]),
    );
    let pad_x = ((target_w as u32).saturating_sub(new_w)) / 2;
    let pad_y = ((target_h as u32).saturating_sub(new_h)) / 2;
    image::imageops::overlay(&mut canvas, &resized.to_rgb8(), pad_x.into(), pad_y.into());
    (
        DynamicImage::ImageRgb8(canvas),
        scale,
        pad_x as f32,
        pad_y as f32,
    )
}

fn image_to_tensor(image: &DynamicImage) -> anyhow::Result<Tensor> {
    let rgb = image.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let mut data = Vec::with_capacity(w * h * 3);
    for pixel in rgb.pixels() {
        data.push(pixel[0] as f32 / 255.0);
        data.push(pixel[1] as f32 / 255.0);
        data.push(pixel[2] as f32 / 255.0);
    }
    let tensor = tract_ndarray::Array4::from_shape_vec((1, h, w, 3), data)?
        .permuted_axes((0, 3, 1, 2))
        .into_tensor();
    Ok(tensor)
}

fn nms(mut dets: Vec<OnnxDetection>, iou_threshold: f32) -> Vec<OnnxDetection> {
    dets.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<OnnxDetection> = Vec::new();
    'outer: for det in dets {
        for kept_det in &kept {
            if iou(det.rect, kept_det.rect) > iou_threshold {
                continue 'outer;
            }
        }
        kept.push(det);
    }
    kept
}

fn iou(a: RectF, b: RectF) -> f32 {
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
