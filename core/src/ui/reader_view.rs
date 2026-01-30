extern crate alloc;

use alloc::vec::Vec;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions};

use crate::image_viewer::ImageData;

use super::geom::Rect;
use super::view::{RenderQueue, UiContext, View};

pub struct ReaderView<'a> {
    pub image: &'a ImageData,
    pub refresh: crate::display::RefreshMode,
}

impl<'a> ReaderView<'a> {
    pub fn new(image: &'a ImageData) -> Self {
        Self {
            image,
            refresh: crate::display::RefreshMode::Full,
        }
    }
}

impl View for ReaderView<'_> {
    fn render(&mut self, ctx: &mut UiContext<'_>, rect: Rect, rq: &mut RenderQueue) {
        render_image(ctx, self.image);
        rq.push(rect, self.refresh);
    }
}

fn render_image(ctx: &mut UiContext<'_>, image: &ImageData) {
    ctx.buffers.clear(BinaryColor::On).ok();
    match image {
        ImageData::Gray2Planes {
            width,
            height,
            lsb,
            msb,
        } => render_gray2_fallback(ctx, *width, *height, lsb, msb),
        ImageData::Mono1 {
            width,
            height,
            bits,
        } => render_mono1(ctx, *width, *height, bits),
        ImageData::Gray8 {
            width,
            height,
            pixels,
        } => render_gray8(ctx, *width, *height, pixels),
    }
}

fn render_gray2_fallback(ctx: &mut UiContext<'_>, width: u32, height: u32, lsb: &[u8], msb: &[u8]) {
    let mut pixels = Vec::with_capacity((width as usize).saturating_mul(height as usize));
    let total = (width as usize).saturating_mul(height as usize);
    for i in 0..total {
        let byte = i / 8;
        let bit = 7 - (i % 8);
        let l = if byte < lsb.len() { (lsb[byte] >> bit) & 0x01 } else { 0 };
        let m = if byte < msb.len() { (msb[byte] >> bit) & 0x01 } else { 0 };
        let level = (m << 1) | l;
        let lum = match level {
            0 => 255,
            1 => 85,
            2 => 170,
            _ => 0,
        };
        pixels.push(lum);
    }
    render_gray8(ctx, width, height, &pixels);
}

fn render_mono1(ctx: &mut UiContext<'_>, width: u32, height: u32, bits: &[u8]) {
    let target = ctx.buffers.size();
    let target_w = target.width.max(1);
    let target_h = target.height.max(1);

    let src_w = width.max(1) as usize;
    let src_h = height.max(1) as usize;
    for y in 0..target_h {
        let src_y = (y as u64 * src_h as u64 / target_h as u64) as usize;
        for x in 0..target_w {
            let src_x = (x as u64 * src_w as u64 / target_w as u64) as usize;
            let idx = src_y * src_w + src_x;
            let byte = idx / 8;
            if byte >= bits.len() {
                continue;
            }
            let bit = 7 - (idx % 8);
            let white = (bits[byte] >> bit) & 0x01 == 1;
            ctx.buffers.set_pixel(
                x as i32,
                y as i32,
                if white { BinaryColor::On } else { BinaryColor::Off },
            );
        }
    }
}

fn render_gray8(ctx: &mut UiContext<'_>, width: u32, height: u32, pixels: &[u8]) {
    let target = ctx.buffers.size();
    let target_w = target.width.max(1);
    let target_h = target.height.max(1);
    let img_w = width.max(1);
    let img_h = height.max(1);

    let (scaled_w, scaled_h) = if img_w * target_h > img_h * target_w {
        let h = (img_h as u64 * target_w as u64 / img_w as u64) as u32;
        (target_w, h.max(1))
    } else {
        let w = (img_w as u64 * target_h as u64 / img_h as u64) as u32;
        (w.max(1), target_h)
    };

    let offset_x = ((target_w - scaled_w) / 2) as i32;
    let offset_y = ((target_h - scaled_h) / 2) as i32;

    let bayer: [[u8; 4]; 4] = [
        [0, 8, 2, 10],
        [12, 4, 14, 6],
        [3, 11, 1, 9],
        [15, 7, 13, 5],
    ];

    for y in 0..scaled_h {
        let src_y = (y as u64 * img_h as u64 / scaled_h as u64) as usize;
        for x in 0..scaled_w {
            let src_x = (x as u64 * img_w as u64 / scaled_w as u64) as usize;
            let idx = src_y * img_w as usize + src_x;
            if idx >= pixels.len() {
                continue;
            }
            let lum = pixels[idx];
            let threshold = (bayer[(y as usize) & 3][(x as usize) & 3] * 16 + 8) as u8;
            let color = if lum < threshold {
                BinaryColor::Off
            } else {
                BinaryColor::On
            };
            ctx.buffers
                .set_pixel(offset_x + x as i32, offset_y + y as i32, color);
        }
    }
}
