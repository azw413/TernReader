extern crate alloc;

use alloc::{collections::BTreeMap, format, string::String};
use alloc::vec::Vec;

use embedded_graphics::{
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point},
    text::Text,
    Drawable,
};

use crate::display::{Display, GrayscaleMode, RefreshMode};
use crate::framebuffer::{DisplayBuffers, Rotation, BUFFER_SIZE, HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH};
use crate::image_viewer::{AppSource, ImageData, ImageError};
use crate::input;
use crate::ui::{flush_queue, ListItem, ListView, Rect, RenderQueue, UiContext, View};

const LIST_TOP: i32 = 60;
const LINE_HEIGHT: i32 = 24;
const LIST_MARGIN_X: i32 = 16;
const HEADER_Y: i32 = 24;
const BOOK_FULL_REFRESH_EVERY: usize = 10;

#[derive(Clone, Copy, Debug)]
pub enum PageTurnIndicator {
    Forward,
    Backward,
}

pub struct BookReaderState {
    pub current_book: Option<crate::trbk::TrbkBookInfo>,
    pub current_page_ops: Option<crate::trbk::TrbkPage>,
    pub next_page_ops: Option<crate::trbk::TrbkPage>,
    pub prefetched_page: Option<usize>,
    pub prefetched_gray2_used: bool,
    pub toc_selected: usize,
    pub toc_labels: Option<Vec<String>>,
    pub current_page: usize,
    pub book_turns_since_full: usize,
    pub last_rendered_page: Option<usize>,
    pub page_turn_indicator: Option<PageTurnIndicator>,
}

pub struct BookReaderContext<'a, S: AppSource> {
    pub display_buffers: &'a mut DisplayBuffers,
    pub gray2_lsb: &'a mut [u8],
    pub gray2_msb: &'a mut [u8],
    pub source: &'a mut S,
    pub full_refresh: &'a mut bool,
}

pub struct BookViewResult {
    pub exit: bool,
    pub open_toc: bool,
    pub dirty: bool,
}

pub struct TocResult {
    pub exit: bool,
    pub jumped: bool,
    pub dirty: bool,
}

impl BookReaderState {
    pub fn new() -> Self {
        Self {
            current_book: None,
            current_page_ops: None,
            next_page_ops: None,
            prefetched_page: None,
            prefetched_gray2_used: false,
            toc_selected: 0,
            toc_labels: None,
            current_page: 0,
            book_turns_since_full: 0,
            last_rendered_page: None,
            page_turn_indicator: None,
        }
    }

    pub fn clear(&mut self) {
        self.current_book = None;
        self.current_page_ops = None;
        self.next_page_ops = None;
        self.prefetched_page = None;
        self.prefetched_gray2_used = false;
        self.toc_selected = 0;
        self.toc_labels = None;
        self.current_page = 0;
        self.book_turns_since_full = 0;
        self.last_rendered_page = None;
        self.page_turn_indicator = None;
    }

    pub fn close<S: AppSource>(&mut self, source: &mut S) {
        self.clear();
        source.close_trbk();
    }

    pub fn open<S: AppSource>(
        &mut self,
        source: &mut S,
        path: &[String],
        entry: &crate::image_viewer::ImageEntry,
        entry_name: &str,
        book_positions: &BTreeMap<String, usize>,
    ) -> Result<(), ImageError> {
        let info = source.open_trbk(path, entry)?;
        self.current_book = Some(info);
        self.toc_labels = None;
        self.current_page = book_positions.get(entry_name).copied().unwrap_or(0);
        self.current_page_ops = source.trbk_page(self.current_page).ok();
        self.next_page_ops = None;
        self.prefetched_page = None;
        self.prefetched_gray2_used = false;
        self.last_rendered_page = None;
        self.book_turns_since_full = 0;
        Ok(())
    }

    pub fn has_book(&self) -> bool {
        self.current_book.is_some()
    }

    pub fn take_page_turn_indicator(&mut self) -> Option<PageTurnIndicator> {
        self.page_turn_indicator.take()
    }

    pub fn handle_view_input<S: AppSource>(
        &mut self,
        source: &mut S,
        buttons: &input::ButtonState,
    ) -> BookViewResult {
        let mut result = BookViewResult {
            exit: false,
            open_toc: false,
            dirty: false,
        };

        if buttons.is_pressed(input::Buttons::Left)
            || buttons.is_pressed(input::Buttons::Up)
        {
            if self.current_page > 0 {
                self.current_page = self.current_page.saturating_sub(1);
                self.current_page_ops = None;
                self.next_page_ops = None;
                self.prefetched_page = None;
                self.prefetched_gray2_used = false;
                self.book_turns_since_full = self.book_turns_since_full.saturating_add(1);
                self.page_turn_indicator = Some(PageTurnIndicator::Backward);
                result.dirty = true;
            }
            return result;
        }

        if buttons.is_pressed(input::Buttons::Right)
            || buttons.is_pressed(input::Buttons::Down)
        {
            if let Some(book) = &self.current_book {
                if self.current_page + 1 < book.page_count {
                    self.current_page += 1;
                    if let Some(next_ops) = self.next_page_ops.take() {
                        self.current_page_ops = Some(next_ops);
                    } else {
                        self.current_page_ops = None;
                    }
                    self.next_page_ops = None;
                    self.prefetched_page = None;
                    self.prefetched_gray2_used = false;
                    self.book_turns_since_full = self.book_turns_since_full.saturating_add(1);
                    self.page_turn_indicator = Some(PageTurnIndicator::Forward);
                    result.dirty = true;
                }
            }
            return result;
        }

        if buttons.is_pressed(input::Buttons::Confirm) {
            if let Some(book) = &self.current_book {
                if !book.toc.is_empty() {
                    self.toc_selected = find_toc_selection(book, self.current_page);
                    self.toc_labels = None;
                    result.open_toc = true;
                    result.dirty = true;
                }
            }
            return result;
        }

        if buttons.is_pressed(input::Buttons::Back) {
            result.exit = true;
            result.dirty = true;
            return result;
        }

        // Keep source used to avoid unused warnings; may be needed later.
        let _ = source;
        result
    }

    pub fn handle_toc_input(
        &mut self,
        buttons: &input::ButtonState,
    ) -> TocResult {
        let mut result = TocResult {
            exit: false,
            jumped: false,
            dirty: false,
        };

        let Some(book) = &self.current_book else {
            result.exit = true;
            result.dirty = true;
            return result;
        };

        let toc_len = book.toc.len();
        if buttons.is_pressed(input::Buttons::Up) {
            if self.toc_selected > 0 {
                self.toc_selected -= 1;
                result.dirty = true;
            }
            return result;
        }
        if buttons.is_pressed(input::Buttons::Down) {
            if self.toc_selected + 1 < toc_len {
                self.toc_selected += 1;
                result.dirty = true;
            }
            return result;
        }
        if buttons.is_pressed(input::Buttons::Confirm) {
            if let Some(entry) = book.toc.get(self.toc_selected) {
                self.current_page = entry.page_index as usize;
                self.current_page_ops = None;
                self.next_page_ops = None;
                self.prefetched_page = None;
                self.prefetched_gray2_used = false;
                self.last_rendered_page = None;
                self.book_turns_since_full = 0;
                result.jumped = true;
                result.dirty = true;
            }
            return result;
        }
        if buttons.is_pressed(input::Buttons::Back) {
            result.exit = true;
            result.dirty = true;
            return result;
        }

        result
    }

    pub fn draw_toc<S: AppSource>(
        &mut self,
        ctx: &mut BookReaderContext<'_, S>,
        display: &mut impl Display,
    ) -> Result<(), ImageError> {
        ctx.display_buffers.clear(BinaryColor::On).ok();
        let Some(book) = &self.current_book else {
            return Err(ImageError::Decode);
        };
        if self.toc_labels.is_none() {
            let mut labels: Vec<String> = Vec::with_capacity(book.toc.len());
            for entry in &book.toc {
                let mut label = String::new();
                let indent = (entry.level as usize).min(6);
                for _ in 0..indent {
                    label.push_str("  ");
                }
                label.push_str(entry.title.as_str());
                labels.push(label);
            }
            self.toc_labels = Some(labels);
        }
        let labels = self.toc_labels.as_ref().map(Vec::as_slice).unwrap_or(&[]);
        let items: Vec<ListItem<'_>> = labels
            .iter()
            .map(|label| ListItem { label: label.as_str() })
            .collect();

        let title = book.metadata.title.as_str();
        let mut list = ListView::new(&items);
        list.title = Some(title);
        list.footer = Some("Up/Down: select  Confirm: jump  Back: return");
        list.empty_label = Some("No table of contents.");
        list.selected = self.toc_selected.min(items.len().saturating_sub(1));
        list.margin_x = LIST_MARGIN_X;
        list.header_y = HEADER_Y;
        list.list_top = LIST_TOP;
        list.line_height = LINE_HEIGHT;

        let size = ctx.display_buffers.size();
        let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
        let mut rq = RenderQueue::default();
        let mut ui = UiContext {
            buffers: ctx.display_buffers,
        };
        list.render(&mut ui, rect, &mut rq);
        let refresh = if *ctx.full_refresh {
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };
        flush_queue(display, ctx.display_buffers, &mut rq, refresh);
        Ok(())
    }

    pub fn draw_book<S: AppSource>(
        &mut self,
        ctx: &mut BookReaderContext<'_, S>,
        display: &mut impl Display,
    ) -> Result<(), ImageError> {
        let Some(book) = &self.current_book else {
            return Err(ImageError::Decode);
        };
        let book_ptr = book as *const crate::trbk::TrbkBookInfo;
        let book_page_count = book.page_count;
        let using_prefetch = self.prefetched_page == Some(self.current_page);
        let mut gray2_used = false;
        let mut gray2_absolute = false;
        if using_prefetch {
            gray2_used = self.prefetched_gray2_used;
        } else {
            ctx.display_buffers.clear(BinaryColor::On).ok();
            ctx.gray2_lsb.fill(0);
            ctx.gray2_msb.fill(0);
            if self.current_page_ops.is_none() {
                self.current_page_ops = ctx.source.trbk_page(self.current_page).ok();
            }
            let page = self.current_page_ops.clone();
            if let Some(page) = page.as_ref() {
                unsafe {
                    self.render_trbk_page_ops(ctx, &*book_ptr, page, &mut gray2_used, &mut gray2_absolute);
                }
            }
        }
        self.last_rendered_page = Some(self.current_page);
        draw_page_indicator(ctx.display_buffers, self.current_page, book_page_count);
        if self.book_turns_since_full >= BOOK_FULL_REFRESH_EVERY {
            *ctx.full_refresh = true;
            self.book_turns_since_full = 0;
        }
        let mode = if *ctx.full_refresh {
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };
        if gray2_used {
            display.display(ctx.display_buffers, mode);
            let lsb_buf: &[u8; BUFFER_SIZE] = ctx.gray2_lsb.as_ref().try_into().unwrap();
            let msb_buf: &[u8; BUFFER_SIZE] = ctx.gray2_msb.as_ref().try_into().unwrap();
            display.copy_grayscale_buffers(lsb_buf, msb_buf);
            if gray2_absolute {
                display.display_absolute_grayscale(GrayscaleMode::Fast);
            } else {
                display.display_differential_grayscale(false);
            }
        } else {
            let mut rq = RenderQueue::default();
            let size = ctx.display_buffers.size();
            rq.push(Rect::new(0, 0, size.width as i32, size.height as i32), mode);
            flush_queue(display, ctx.display_buffers, &mut rq, mode);
        }

        self.prefetched_page = None;
        self.prefetched_gray2_used = false;

        if self.next_page_ops.is_none() {
            let next = self.current_page + 1;
            if next < book_page_count {
                self.next_page_ops = ctx.source.trbk_page(next).ok();
            }
        }
        unsafe {
            self.prefetch_next_page(ctx, &*book_ptr);
        }
        Ok(())
    }

    fn render_trbk_page_ops<S: AppSource>(
        &mut self,
        ctx: &mut BookReaderContext<'_, S>,
        book: &crate::trbk::TrbkBookInfo,
        page: &crate::trbk::TrbkPage,
        gray2_used: &mut bool,
        gray2_absolute: &mut bool,
    ) {
        for op in &page.ops {
            match op {
                crate::trbk::TrbkOp::TextRun { x, y, style, text } => {
                    let gray2_lsb = &mut *ctx.gray2_lsb;
                    let gray2_msb = &mut *ctx.gray2_msb;
                    let mut gray2_ctx = Some((gray2_lsb, gray2_msb, &mut *gray2_used));
                    draw_trbk_text(
                        ctx.display_buffers,
                        book,
                        &mut gray2_ctx,
                        *x,
                        *y,
                        *style,
                        text,
                    );
                }
                crate::trbk::TrbkOp::Image {
                    x,
                    y,
                    width,
                    height,
                    image_index,
                } => {
                    let op_w = *width as u32;
                    let op_h = *height as u32;
                    match ctx.source.trbk_image(*image_index as usize) {
                        Ok(image) => {
                            match &image {
                                ImageData::Gray2Stream { width, height, key } => {
                                    let size = ctx.display_buffers.size();
                                    if *x == 0
                                        && *y == 0
                                        && op_w == size.width
                                        && op_h == size.height
                                        && *width == op_w
                                        && *height == op_h
                                    {
                                        let rotation = ctx.display_buffers.rotation();
                                        let base_buf = ctx.display_buffers.get_active_buffer_mut();
                                        base_buf.fill(0xFF);
                                        if ctx
                                            .source
                                            .load_gray2_stream(
                                                key,
                                                *width,
                                                *height,
                                                rotation,
                                                base_buf,
                                                &mut *ctx.gray2_lsb,
                                                &mut *ctx.gray2_msb,
                                            )
                                            .is_ok()
                                        {
                                            *gray2_used = true;
                                            *gray2_absolute = true;
                                        } else {
                                            log::warn!(
                                                "Gray2 stream load failed for image {} ({}x{})",
                                                image_index,
                                                width,
                                                height
                                            );
                                        }
                                    } else if *width == op_w && *height == op_h {
                                        let rotation = ctx.display_buffers.rotation();
                                        let base_buf = ctx.display_buffers.get_active_buffer_mut();
                                        if ctx
                                            .source
                                            .load_gray2_stream_region(
                                                key,
                                                *width,
                                                *height,
                                                rotation,
                                                base_buf,
                                                &mut *ctx.gray2_lsb,
                                                &mut *ctx.gray2_msb,
                                                *x,
                                                *y,
                                            )
                                            .is_ok()
                                        {
                                            *gray2_used = true;
                                        } else {
                                            log::warn!(
                                                "Gray2 stream region load failed for image {} ({}x{})",
                                                image_index,
                                                width,
                                                height
                                            );
                                        }
                                    } else {
                                        log::warn!(
                                            "Gray2 stream skipped (non-fullscreen) image {} at ({}, {}) size {}x{}",
                                            image_index,
                                            x,
                                            y,
                                            width,
                                            height
                                        );
                                    }
                                }
                                _ => {
                                    let gray2_lsb = &mut *ctx.gray2_lsb;
                                    let gray2_msb = &mut *ctx.gray2_msb;
                                    let mut gray2_ctx =
                                        Some((gray2_lsb, gray2_msb, &mut *gray2_used));
                                    draw_trbk_image(
                                        ctx.display_buffers,
                                        &image,
                                        &mut gray2_ctx,
                                        *x,
                                        *y,
                                        *width as i32,
                                        *height as i32,
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            log::warn!(
                                "Failed to load TRBK image {} ({}x{}): {:?}",
                                image_index,
                                width,
                                height,
                                err
                            );
                        }
                    }
                }
            }
        }
    }

    fn prefetch_next_page<S: AppSource>(
        &mut self,
        ctx: &mut BookReaderContext<'_, S>,
        book: &crate::trbk::TrbkBookInfo,
    ) {
        if self.prefetched_page.is_some() {
            return;
        }
        let next = self.current_page + 1;
        if next >= book.page_count {
            return;
        }
        if self.next_page_ops.is_none() {
            self.next_page_ops = ctx.source.trbk_page(next).ok();
        }
        let Some(page) = self.next_page_ops.clone() else {
            return;
        };
        ctx.display_buffers.clear(BinaryColor::On).ok();
        ctx.gray2_lsb.fill(0);
        ctx.gray2_msb.fill(0);
        let mut gray2_used = false;
        let mut gray2_absolute = false;
        self.render_trbk_page_ops(ctx, book, &page, &mut gray2_used, &mut gray2_absolute);
        draw_page_indicator(ctx.display_buffers, next, book.page_count);
        if gray2_absolute {
            self.prefetched_page = None;
            self.prefetched_gray2_used = false;
            return;
        }
        self.prefetched_page = Some(next);
        self.prefetched_gray2_used = gray2_used;
    }
}

fn draw_trbk_text(
    buffers: &mut DisplayBuffers,
    book: &crate::trbk::TrbkBookInfo,
    gray2: &mut Option<(&mut [u8], &mut [u8], &mut bool)>,
    x: i32,
    y: i32,
    style: u8,
    text: &str,
) {
    if book.glyphs.is_empty() {
        let fallback = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new(text, Point::new(x, y), fallback)
            .draw(buffers)
            .ok();
        return;
    }

    let mut pen_x = x;
    let baseline = y;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            continue;
        }
        let codepoint = ch as u32;
        if let Some(glyph) = find_glyph(book.glyphs.as_slice(), style, codepoint) {
            draw_glyph(buffers, glyph, gray2, pen_x, baseline);
            pen_x += glyph.x_advance as i32;
        } else {
            pen_x += book.metadata.char_width as i32;
        }
    }
}

pub(crate) fn draw_trbk_image(
    buffers: &mut DisplayBuffers,
    image: &ImageData,
    gray2: &mut Option<(&mut [u8], &mut [u8], &mut bool)>,
    x: i32,
    y: i32,
    target_w: i32,
    target_h: i32,
) {
    match image {
        ImageData::Mono1 {
            width,
            height,
            bits,
        } => {
            let src_w = *width as i32;
            let src_h = *height as i32;
            let dst_w = target_w.max(1);
            let dst_h = target_h.max(1);
            for ty in 0..dst_h {
                let src_y = (ty as i64 * src_h as i64 / dst_h as i64) as i32;
                for tx in 0..dst_w {
                    let src_x = (tx as i64 * src_w as i64 / dst_w as i64) as i32;
                    if src_x < 0 || src_y < 0 {
                        continue;
                    }
                    let idx = (src_y as usize) * (*width as usize) + src_x as usize;
                    let byte = idx / 8;
                    if byte >= bits.len() {
                        continue;
                    }
                    let bit = 7 - (idx % 8);
                    let white = (bits[byte] >> bit) & 0x01 == 1;
                    buffers.set_pixel(
                        x + tx,
                        y + ty,
                        if white {
                            BinaryColor::On
                        } else {
                            BinaryColor::Off
                        },
                    );
                }
            }
        }
        ImageData::Gray8 {
            width,
            height,
            pixels,
        } => {
            let src_w = *width as i32;
            let src_h = *height as i32;
            let dst_w = target_w.max(1);
            let dst_h = target_h.max(1);
            let bayer: [[u8; 4]; 4] = [
                [0, 8, 2, 10],
                [12, 4, 14, 6],
                [3, 11, 1, 9],
                [15, 7, 13, 5],
            ];
            for ty in 0..dst_h {
                let src_y = (ty as i64 * src_h as i64 / dst_h as i64) as i32;
                for tx in 0..dst_w {
                    let src_x = (tx as i64 * src_w as i64 / dst_w as i64) as i32;
                    let idx = (src_y as usize) * (*width as usize) + src_x as usize;
                    if idx >= pixels.len() {
                        continue;
                    }
                    let lum = pixels[idx];
                    let threshold = (bayer[(ty as usize) & 3][(tx as usize) & 3] * 16 + 8)
                        as u8;
                    let color = if lum < threshold {
                        BinaryColor::Off
                    } else {
                        BinaryColor::On
                    };
                    buffers.set_pixel(x + tx, y + ty, color);
                }
            }
        }
        ImageData::Gray2 {
            width,
            height,
            data,
        } => {
            let plane = ((*width as usize * *height as usize) + 7) / 8;
            if data.len() < plane * 3 {
                return;
            }
            let base = &data[..plane];
            let lsb = &data[plane..plane * 2];
            let msb = &data[plane * 2..plane * 3];
            let Some((gray2_lsb, gray2_msb, gray2_used)) = gray2.as_mut() else {
                return;
            };
            **gray2_used = true;
            let src_w = *width as i32;
            let src_h = *height as i32;
            let dst_w = target_w.max(1);
            let dst_h = target_h.max(1);
            for ty in 0..dst_h {
                let src_y = (ty as i64 * src_h as i64 / dst_h as i64) as i32;
                for tx in 0..dst_w {
                    let src_x = (tx as i64 * src_w as i64 / dst_w as i64) as i32;
                    if src_x < 0 || src_y < 0 {
                        continue;
                    }
                    let idx = (src_y as usize) * (*width as usize) + src_x as usize;
                    let byte = idx / 8;
                    if byte >= base.len() || byte >= lsb.len() || byte >= msb.len() {
                        continue;
                    }
                    let bit = 7 - (idx % 8);
                    let base_white = (base[byte] >> bit) & 0x01 == 1;
                    buffers.set_pixel(
                        x + tx,
                        y + ty,
                        if base_white {
                            BinaryColor::On
                        } else {
                            BinaryColor::Off
                        },
                    );
                    let dst_x = x + tx;
                    let dst_y = y + ty;
                    let Some((fx, fy)) =
                        map_display_point(buffers.rotation(), dst_x, dst_y)
                    else {
                        continue;
                    };
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
        ImageData::Gray2Stream { .. } => {}
    }
}

fn draw_page_indicator(buffers: &mut DisplayBuffers, page: usize, total: usize) {
    if total == 0 {
        return;
    }
    let label = format!("{}/{}", page.saturating_add(1), total);
    let text_w = (label.len() as i32) * 10;
    let size = buffers.size();
    let margin = 8;
    let x = (size.width as i32 - margin - text_w).max(margin);
    let y = (size.height as i32 - margin).max(0);
    let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
    Text::new(label.as_str(), Point::new(x, y), style)
        .draw(buffers)
        .ok();
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

fn find_glyph<'a>(
    glyphs: &'a [crate::trbk::TrbkGlyph],
    style: u8,
    codepoint: u32,
) -> Option<&'a crate::trbk::TrbkGlyph> {
    glyphs
        .iter()
        .find(|glyph| glyph.style == style && glyph.codepoint == codepoint)
}

pub fn find_toc_selection(book: &crate::trbk::TrbkBookInfo, page: usize) -> usize {
    let mut selected = 0usize;
    for (idx, entry) in book.toc.iter().enumerate() {
        if (entry.page_index as usize) <= page {
            selected = idx;
        } else {
            break;
        }
    }
    selected
}

fn draw_glyph(
    buffers: &mut DisplayBuffers,
    glyph: &crate::trbk::TrbkGlyph,
    gray2: &mut Option<(&mut [u8], &mut [u8], &mut bool)>,
    origin_x: i32,
    baseline: i32,
) {
    let width = glyph.width as i32;
    let height = glyph.height as i32;
    if width == 0 || height == 0 {
        return;
    }
    let start_x = origin_x + glyph.x_offset as i32;
    let start_y = baseline - glyph.y_offset as i32;
    let rotation = buffers.rotation();
    let mut idx = 0usize;
    let has_gray2 = glyph.bitmap_lsb.is_some() && glyph.bitmap_msb.is_some();
    for row in 0..height {
        for col in 0..width {
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            if byte < glyph.bitmap_bw.len() {
                let bw_set = (glyph.bitmap_bw[byte] & (1 << bit)) != 0;
                let draw_black = if has_gray2 { !bw_set } else { bw_set };
                if draw_black {
                    buffers.set_pixel(start_x + col, start_y + row, BinaryColor::Off);
                }
            }
            if let (Some(lsb), Some(msb)) =
                (glyph.bitmap_lsb.as_ref(), glyph.bitmap_msb.as_ref())
            {
                if let Some((gray2_lsb, gray2_msb, gray2_used)) = gray2.as_mut() {
                    **gray2_used = true;
                    if byte < lsb.len() && (lsb[byte] & (1 << bit)) != 0 {
                        if let Some((fx, fy)) =
                            map_display_point(rotation, start_x + col, start_y + row)
                        {
                            let dst_idx = fy * FB_WIDTH + fx;
                            let dst_byte = dst_idx / 8;
                            let dst_bit = 7 - (dst_idx % 8);
                            gray2_lsb[dst_byte] |= 1 << dst_bit;
                        }
                    }
                    if byte < msb.len() && (msb[byte] & (1 << bit)) != 0 {
                        if let Some((fx, fy)) =
                            map_display_point(rotation, start_x + col, start_y + row)
                        {
                            let dst_idx = fy * FB_WIDTH + fx;
                            let dst_byte = dst_idx / 8;
                            let dst_bit = 7 - (dst_idx % 8);
                            gray2_msb[dst_byte] |= 1 << dst_bit;
                        }
                    }
                }
            }
            idx += 1;
        }
    }
}
