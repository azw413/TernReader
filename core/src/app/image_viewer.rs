extern crate alloc;

use embedded_graphics::draw_target::DrawTarget;
use embedded_graphics::geometry::OriginDimensions;
use embedded_graphics::pixelcolor::BinaryColor;

use alloc::string::String;

use crate::display::{Display, GrayscaleMode, RefreshMode};
use crate::framebuffer::{DisplayBuffers, Rotation, BUFFER_SIZE, HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH};
use crate::image_viewer::{AppSource, ImageData, ImageEntry, ImageError};
use crate::ui::{flush_queue, Rect, RenderQueue, UiContext, ReaderView, View};

const DEBUG_GRAY2_MODE: u8 = 0; // 0=normal, 1=base, 2=lsb, 3=msb

pub struct ImageViewerState {
    current_image: Option<ImageData>,
}

pub struct ImageViewerContext<'a, S: AppSource> {
    pub display_buffers: &'a mut DisplayBuffers,
    pub gray2_lsb: &'a mut [u8],
    pub gray2_msb: &'a mut [u8],
    pub source: &'a mut S,
    pub wake_restore_only: &'a mut bool,
}

impl ImageViewerState {
    pub fn new() -> Self {
        Self { current_image: None }
    }

    pub fn set_image(&mut self, image: ImageData) {
        self.current_image = Some(image);
    }

    pub fn open<S: AppSource>(
        &mut self,
        source: &mut S,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<(), ImageError> {
        let image = source.load(path, entry)?;
        self.current_image = Some(image);
        Ok(())
    }

    pub fn clear(&mut self) {
        self.current_image = None;
    }

    pub fn has_image(&self) -> bool {
        self.current_image.is_some()
    }

    pub fn take_image(&mut self) -> Option<ImageData> {
        self.current_image.take()
    }

    pub fn restore_image(&mut self, image: ImageData) {
        self.current_image = Some(image);
    }

    pub fn draw<S: AppSource>(
        &mut self,
        ctx: &mut ImageViewerContext<'_, S>,
        display: &mut impl Display,
    ) -> Result<(), ImageError> {
        if *ctx.wake_restore_only {
            *ctx.wake_restore_only = false;
            let size = ctx.display_buffers.size();
            let mut rq = RenderQueue::default();
            rq.push(
                Rect::new(0, 0, size.width as i32, size.height as i32),
                RefreshMode::Fast,
            );
            flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Fast);
            return Ok(());
        }

        let Some(image) = self.current_image.take() else {
            return Err(ImageError::Decode);
        };

        match &image {
            ImageData::Gray2 {
                width,
                height,
                data,
            } => {
                let plane = ((*width as usize * *height as usize) + 7) / 8;
                if data.len() < plane * 3 {
                    return Ok(());
                }
                let base = &data[..plane];
                let lsb = &data[plane..plane * 2];
                let msb = &data[plane * 2..plane * 3];
                ctx.display_buffers.clear(BinaryColor::On).ok();
                ctx.gray2_lsb.fill(0);
                ctx.gray2_msb.fill(0);
                render_gray2_contain(
                    ctx.display_buffers,
                    ctx.display_buffers.rotation(),
                    ctx.gray2_lsb,
                    ctx.gray2_msb,
                    *width,
                    *height,
                    base,
                    lsb,
                    msb,
                );
                ctx.display_buffers.copy_active_to_inactive();
                if DEBUG_GRAY2_MODE != 0 {
                    apply_gray2_debug_overlay(
                        ctx.display_buffers,
                        ctx.gray2_lsb,
                        ctx.gray2_msb,
                        DEBUG_GRAY2_MODE,
                    );
                    display.display(ctx.display_buffers, RefreshMode::Full);
                } else {
                    let lsb_buf: &[u8; BUFFER_SIZE] = ctx.gray2_lsb.as_ref().try_into().unwrap();
                    let msb_buf: &[u8; BUFFER_SIZE] = ctx.gray2_msb.as_ref().try_into().unwrap();
                    display.copy_grayscale_buffers(lsb_buf, msb_buf);
                    display.display_absolute_grayscale(GrayscaleMode::Fast);
                }
            }
            ImageData::Gray2Stream { width, height, key } => {
                let plane = ((*width as usize * *height as usize) + 7) / 8;
                if plane > BUFFER_SIZE {
                    return Err(ImageError::Message(
                        "Image size not supported on device.".into(),
                    ));
                }
                let rotation = ctx.display_buffers.rotation();
                let size = ctx.display_buffers.size();
                if *width != size.width || *height != size.height {
                    return Err(ImageError::Message(
                        "Grayscale images must match display size.".into(),
                    ));
                }
                let base_buf = ctx.display_buffers.get_active_buffer_mut();
                base_buf.fill(0xFF);
                ctx.gray2_lsb.fill(0);
                ctx.gray2_msb.fill(0);
                if ctx
                    .source
                    .load_gray2_stream(
                        key,
                        *width,
                        *height,
                        rotation,
                        base_buf,
                        ctx.gray2_lsb,
                        ctx.gray2_msb,
                    )
                    .is_err()
                {
                    return Err(ImageError::Decode);
                }
                ctx.display_buffers.copy_active_to_inactive();
                if DEBUG_GRAY2_MODE != 0 {
                    apply_gray2_debug_overlay(
                        ctx.display_buffers,
                        ctx.gray2_lsb,
                        ctx.gray2_msb,
                        DEBUG_GRAY2_MODE,
                    );
                    display.display(ctx.display_buffers, RefreshMode::Full);
                } else {
                    let lsb_buf: &[u8; BUFFER_SIZE] = ctx.gray2_lsb.as_ref().try_into().unwrap();
                    let msb_buf: &[u8; BUFFER_SIZE] = ctx.gray2_msb.as_ref().try_into().unwrap();
                    display.copy_grayscale_buffers(lsb_buf, msb_buf);
                    display.display_absolute_grayscale(GrayscaleMode::Fast);
                }
            }
            _ => {
                let size = ctx.display_buffers.size();
                let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
                let mut rq = RenderQueue::default();
                let mut ctx_ui = UiContext {
                    buffers: ctx.display_buffers,
                };
                let mut reader = ReaderView::new(&image);
                reader.refresh = RefreshMode::Full;
                reader.render(&mut ctx_ui, rect, &mut rq);
                flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Full);
            }
        }

        self.current_image = Some(image);
        Ok(())
    }
}

fn render_gray2_contain(
    buffers: &mut DisplayBuffers,
    rotation: Rotation,
    gray2_lsb: &mut [u8],
    gray2_msb: &mut [u8],
    width: u32,
    height: u32,
    base: &[u8],
    lsb: &[u8],
    msb: &[u8],
) {
    let target = buffers.size();
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

    for y in 0..scaled_h {
        let src_y = (y as u64 * img_h as u64 / scaled_h as u64) as usize;
        for x in 0..scaled_w {
            let src_x = (x as u64 * img_w as u64 / scaled_w as u64) as usize;
            let idx = src_y * img_w as usize + src_x;
            let byte = idx / 8;
            if byte >= base.len() || byte >= lsb.len() || byte >= msb.len() {
                continue;
            }
            let bit = 7 - (idx % 8);
            let dst_x = offset_x + x as i32;
            let dst_y = offset_y + y as i32;
            let Some((fx, fy)) = map_display_point(rotation, dst_x, dst_y) else {
                continue;
            };
            let base_white = (base[byte] >> bit) & 0x01 == 1;
            buffers.set_pixel(
                dst_x,
                dst_y,
                if base_white {
                    BinaryColor::On
                } else {
                    BinaryColor::Off
                },
            );

            let dst_idx = fy * FB_WIDTH + fx;
            let dst_byte = dst_idx / 8;
            let dst_bit = 7 - (dst_idx % 8);
            if (lsb[byte] >> bit) & 0x01 == 1 {
                gray2_lsb[dst_byte] |= 1 << dst_bit;
            }
            if (msb[byte] >> bit) & 0x01 == 1 {
                gray2_msb[dst_byte] |= 1 << dst_bit;
            }
        }
    }
}

fn apply_gray2_debug_overlay(
    buffers: &mut DisplayBuffers,
    gray2_lsb: &[u8],
    gray2_msb: &[u8],
    mode: u8,
) {
    if mode == 0 {
        return;
    }
    let active = buffers.get_active_buffer_mut();
    match mode {
        1 => {}
        2 => {
            for (dst, src) in active.iter_mut().zip(gray2_lsb.iter()) {
                *dst = !*src;
            }
        }
        3 => {
            for (dst, src) in active.iter_mut().zip(gray2_msb.iter()) {
                *dst = !*src;
            }
        }
        _ => {}
    }
}

fn map_display_point(rotation: Rotation, x: i32, y: i32) -> Option<(usize, usize)> {
    if x < 0 || y < 0 {
        return None;
    }
    let (x, y) = match rotation {
        Rotation::Rotate0 => (x as usize, y as usize),
        Rotation::Rotate90 => (y as usize, FB_HEIGHT - 1 - x as usize),
        Rotation::Rotate180 => (FB_WIDTH - 1 - x as usize, FB_HEIGHT - 1 - y as usize),
        Rotation::Rotate270 => (FB_WIDTH - 1 - y as usize, x as usize),
    };
    if x >= FB_WIDTH || y >= FB_HEIGHT {
        None
    } else {
        Some((x, y))
    }
}
