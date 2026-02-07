extern crate alloc;

use alloc::{collections::BTreeMap, string::{String, ToString}, vec::Vec};

use embedded_graphics::{
    Drawable,
    mono_font::{ascii::FONT_10X20, MonoTextStyle},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point, Primitive},
    geometry::Size,
    primitives::{PrimitiveStyle, Rectangle},
    text::Text,
};

use crate::{
    app::{
        book_reader::BookReaderState,
        home::draw_icon_gray2,
        image_viewer::ImageViewerState,
    },
    display::{GrayscaleMode, RefreshMode},
    framebuffer::{DisplayBuffers, Rotation, BUFFER_SIZE, HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH},
    image_viewer::{AppSource, EntryKind, ImageData, ImageEntry},
    ui::{flush_queue, ReaderView, Rect, RenderQueue, UiContext, View},
};

pub struct SleepOverlay {
    pub rect: Rect,
    pub pixels: Vec<u8>,
}

pub struct SleepWallpaperIcons<'a> {
    pub logo_w: i32,
    pub logo_h: i32,
    pub logo_dark: &'a [u8],
    pub logo_light: &'a [u8],
}

pub struct SystemRenderContext<'a, S: AppSource> {
    pub display_buffers: &'a mut DisplayBuffers,
    pub gray2_lsb: &'a mut [u8],
    pub gray2_msb: &'a mut [u8],
    pub source: &'a mut S,
    pub image_viewer: &'a mut ImageViewerState,
    pub book_reader: &'a mut BookReaderState,
    pub last_viewed_entry: &'a Option<String>,
    pub is_start_menu: bool,
    pub logo: SleepWallpaperIcons<'a>,
}

pub struct ResumeContext<'a, S: AppSource> {
    pub source: &'a mut S,
    pub resume_debug: &'a str,
    pub in_start_menu: bool,
    pub current_entry: Option<&'a String>,
    pub last_viewed_entry: Option<&'a String>,
    pub home_current_entry: Option<String>,
    pub book_reader: &'a BookReaderState,
}

pub enum SaveResumeOutcome {
    Ok,
    Error(String),
}

pub enum TryResumeOutcome {
    None,
    Resume {
        path: Vec<String>,
        file: String,
        page: Option<usize>,
    },
}

pub struct SystemState {
    pub sleep_transition: bool,
    pub wake_transition: bool,
    pub full_refresh: bool,
    pub sleep_after_error: bool,
    pub idle_ms: u32,
    pub idle_timeout_ms: u32,
    pub sleep_overlay: Option<SleepOverlay>,
    pub sleep_overlay_pending: bool,
    pub wake_restore_only: bool,
    pub resume_name: Option<String>,
    pub book_positions: BTreeMap<String, usize>,
    pub recent_entries: Vec<String>,
    pub recent_dirty: bool,
    pub book_positions_dirty: bool,
    pub last_saved_resume: Option<String>,
    pub sleep_from_home: bool,
    pub sleep_wallpaper_gray2: bool,
    pub sleep_wallpaper_trbk_open: bool,
    pub battery_percent: Option<u8>,
}

impl SystemState {
    pub fn new(
        resume_name: Option<String>,
        book_positions: BTreeMap<String, usize>,
        recent_entries: Vec<String>,
    ) -> Self {
        Self {
            sleep_transition: true,
            wake_transition: false,
            full_refresh: true,
            sleep_after_error: false,
            idle_ms: 0,
            idle_timeout_ms: 300_000,
            sleep_overlay: None,
            sleep_overlay_pending: false,
            wake_restore_only: false,
            resume_name,
            book_positions,
            recent_entries,
            recent_dirty: false,
            book_positions_dirty: false,
            last_saved_resume: None,
            sleep_from_home: false,
            sleep_wallpaper_gray2: false,
            sleep_wallpaper_trbk_open: false,
            battery_percent: None,
        }
    }

    pub fn reset_idle(&mut self) {
        self.idle_ms = 0;
    }

    pub fn add_idle(&mut self, elapsed_ms: u32) -> bool {
        self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
        self.idle_ms >= self.idle_timeout_ms
    }

    pub fn prepare_sleep(&mut self, from_home: bool) {
        self.sleep_from_home = from_home;
        self.sleep_transition = false;
        self.sleep_overlay_pending = false;
    }

    pub fn start_sleep_request(&mut self, from_home: bool) {
        self.prepare_sleep(from_home);
    }

    pub fn mark_sleep_transition(&mut self) {
        self.sleep_transition = true;
    }

    pub fn mark_wake_transition(&mut self) {
        self.wake_transition = true;
    }

    pub fn clear_sleep_transition(&mut self) {
        self.sleep_transition = false;
    }

    pub fn start_sleep_overlay(&mut self) {
        self.sleep_transition = true;
        self.sleep_overlay_pending = true;
    }

    pub fn clear_sleep_overlay_pending(&mut self) {
        self.sleep_overlay_pending = false;
    }

    pub fn sleep_overlay_pending(&self) -> bool {
        self.sleep_overlay_pending
    }

    pub fn take_sleep_transition(&mut self) -> bool {
        let value = self.sleep_transition;
        self.sleep_transition = false;
        value
    }

    pub fn take_wake_transition(&mut self) -> bool {
        let value = self.wake_transition;
        self.wake_transition = false;
        value
    }

    pub fn set_battery_percent(&mut self, percent: Option<u8>) -> bool {
        if self.battery_percent == percent {
            return false;
        }
        self.battery_percent = percent;
        true
    }

    pub fn on_wake(&mut self) {
        self.mark_wake_transition();
        self.clear_sleep_transition();
        self.full_refresh = true;
        self.reset_idle();
    }

    pub fn collect_recent_paths(&self, last_viewed_entry: Option<&String>) -> Vec<String> {
        let mut recent = self.recent_entries.clone();
        if let Some(entry) = last_viewed_entry {
            if !recent.iter().any(|existing| existing == entry) {
                recent.insert(0, entry.clone());
            }
        }
        for (name, _) in &self.book_positions {
            if recent.len() >= 5 {
                break;
            }
            if !recent.iter().any(|existing| existing == name) {
                recent.push(name.clone());
            }
        }
        recent.truncate(5);
        recent
    }

    pub fn try_resume(&mut self) -> TryResumeOutcome {
        let Some(raw) = self.resume_name.take() else {
            return TryResumeOutcome::None;
        };
        let name = raw;
        if name == "HOME" {
            return TryResumeOutcome::None;
        }
        let mut parts: Vec<String> = name
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| part.to_string())
            .collect();
        if parts.is_empty() {
            return TryResumeOutcome::None;
        }
        let file = parts.pop().unwrap_or_default();
        let page = self.book_positions.get(&name).copied();
        TryResumeOutcome::Resume {
            path: parts,
            file,
            page,
        }
    }

    pub fn mark_recent(&mut self, path: String) {
        self.recent_entries.retain(|entry| entry != &path);
        self.recent_entries.insert(0, path);
        if self.recent_entries.len() > 10 {
            self.recent_entries.truncate(10);
        }
        self.recent_dirty = true;
    }

    pub fn update_book_position(
        &mut self,
        book_reader: &BookReaderState,
        current_entry: Option<&String>,
        last_viewed_entry: Option<&String>,
    ) {
        if book_reader.current_book.is_some() {
            if let Some(name) = current_entry.or(last_viewed_entry) {
                let prev = self.book_positions.insert(name.clone(), book_reader.current_page);
                if prev != Some(book_reader.current_page) {
                    self.book_positions_dirty = true;
                }
            }
        }
    }

    pub fn save_book_positions_now<S: AppSource>(&mut self, source: &mut S) {
        if !self.book_positions_dirty {
            return;
        }
        let entries: Vec<(String, usize)> = self
            .book_positions
            .iter()
            .map(|(name, page)| (name.clone(), *page))
            .collect();
        source.save_book_positions(&entries);
        self.book_positions_dirty = false;
    }

    pub fn save_recent_entries_now<S: AppSource>(&mut self, source: &mut S) {
        if !self.recent_dirty {
            return;
        }
        source.save_recent_entries(&self.recent_entries);
        self.recent_dirty = false;
    }

    pub fn current_resume_string(
        &self,
        in_start_menu: bool,
        current_entry: Option<&String>,
        last_viewed_entry: Option<&String>,
        home_current_entry: Option<String>,
    ) -> Option<String> {
        if in_start_menu {
            return Some("HOME".to_string());
        }
        current_entry
            .cloned()
            .or_else(|| last_viewed_entry.cloned())
            .or(home_current_entry)
    }

    pub fn save_resume_checked<S: AppSource>(&mut self, ctx: ResumeContext<'_, S>) -> SaveResumeOutcome {
        let expected = if self.sleep_from_home {
            Some("HOME".to_string())
        } else {
            self.current_resume_string(
                ctx.in_start_menu,
                ctx.current_entry,
                ctx.last_viewed_entry,
                ctx.home_current_entry,
            )
        };
        let Some(expected) = expected else {
            log::info!("No resume state to save. {}", ctx.resume_debug);
            return SaveResumeOutcome::Ok;
        };
        log::info!("Saving resume state: {} ({})", expected, ctx.resume_debug);
        self.update_book_position(
            ctx.book_reader,
            ctx.current_entry,
            ctx.last_viewed_entry,
        );
        self.save_book_positions_now(ctx.source);
        self.save_recent_entries_now(ctx.source);
        if self.last_saved_resume.as_deref() != Some(expected.as_str()) {
            ctx.source.save_resume(Some(expected.as_str()));
            let actual = ctx.source.load_resume().unwrap_or_default();
            log::info!("Resume state readback: {}", actual);
            self.last_saved_resume = Some(actual.clone());
            if actual.is_empty() || actual != expected {
                self.sleep_after_error = true;
                self.sleep_from_home = false;
                return SaveResumeOutcome::Error("Failed to save resume state.".into());
            }
        }
        SaveResumeOutcome::Ok
    }

    pub fn draw_sleep_overlay<S: AppSource>(
        &mut self,
        ctx: &mut SystemRenderContext<'_, S>,
        display: &mut impl crate::display::Display,
    ) {
        let size = ctx.display_buffers.size();
        let text = "Sleeping...";
        let text_w = (text.len() as i32) * 10;
        let padding = 8;
        let bar_h = 28;
        let bar_w = (text_w + padding * 2).min(size.width as i32);
        let x = ((size.width as i32 - bar_w) / 2).max(0);
        let y = (size.height as i32 - bar_h).max(0);
        let rect = Rect::new(x, y, bar_w, bar_h);

        ctx.display_buffers.clear(BinaryColor::On).ok();
        self.draw_sleep_wallpaper(ctx);

        let saved = Self::save_rect_bits(ctx.display_buffers, rect);
        self.sleep_overlay = Some(SleepOverlay { rect, pixels: saved });

        Rectangle::new(
            Point::new(rect.x, rect.y),
            Size::new(rect.w as u32, rect.h as u32),
        )
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(ctx.display_buffers)
            .ok();

        let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        let text_x = x + padding;
        let text_y = y + bar_h - 14;
        Text::new(text, Point::new(text_x, text_y), style)
            .draw(ctx.display_buffers)
            .ok();

        let mut rq = RenderQueue::default();
        rq.push(
            Rect::new(0, 0, size.width as i32, size.height as i32),
            RefreshMode::Full,
        );
        flush_queue(display, ctx.display_buffers, &mut rq, RefreshMode::Full);
        if self.sleep_wallpaper_gray2 {
            let lsb: &[u8; BUFFER_SIZE] = (&*ctx.gray2_lsb).try_into().unwrap();
            let msb: &[u8; BUFFER_SIZE] = (&*ctx.gray2_msb).try_into().unwrap();
            display.copy_grayscale_buffers(lsb, msb);
            display.display_absolute_grayscale(GrayscaleMode::Fast);
            ctx.display_buffers.copy_active_to_inactive();
        }
    }

    pub fn process_sleep_overlay<S: AppSource>(
        &mut self,
        ctx: &mut SystemRenderContext<'_, S>,
        display: &mut impl crate::display::Display,
    ) {
        if !self.sleep_overlay_pending {
            return;
        }
        self.draw_sleep_overlay(ctx, display);
        ctx.source.sleep();
        self.clear_sleep_overlay_pending();
    }

    pub fn restore_rect_bits(buffers: &mut DisplayBuffers, overlay: &SleepOverlay) {
        let Rect { x, y, w, h } = overlay.rect;
        let mut idx = 0usize;
        for yy in y..y + h {
            for xx in x..x + w {
                let value = overlay.pixels.get(idx).copied().unwrap_or(1);
                let color = if value == 1 {
                    BinaryColor::On
                } else {
                    BinaryColor::Off
                };
                buffers.set_pixel(xx, yy, color);
                idx += 1;
            }
        }
    }

    fn draw_sleep_wallpaper<S: AppSource>(&mut self, ctx: &mut SystemRenderContext<'_, S>) {
        self.sleep_wallpaper_gray2 = false;
        self.sleep_wallpaper_trbk_open = false;
        log::info!(
            "Sleep wallpaper: state_start_menu={} sleep_from_home={} current_image={} current_book={} last_viewed={:?}",
            ctx.is_start_menu,
            self.sleep_from_home,
            ctx.image_viewer.has_image(),
            ctx.book_reader.current_book.is_some(),
            ctx.last_viewed_entry
        );
        if ctx.image_viewer.has_image() {
            if let Some(image) = ctx.image_viewer.take_image() {
                self.render_wallpaper(ctx, &image);
                ctx.image_viewer.restore_image(image);
            }
            return;
        }
        if ctx.book_reader.current_book.is_some() {
            if let Ok(image) = ctx.source.trbk_image(0) {
                self.render_wallpaper(ctx, &image);
            }
            return;
        }
        if ctx.is_start_menu || self.sleep_from_home {
            let recents = self.collect_recent_paths(ctx.last_viewed_entry.as_ref());
            log::info!("Sleep wallpaper recents: {:?}", recents);
            let recents = self.collect_recent_paths(ctx.last_viewed_entry.as_ref());
            if let Some(path) = recents.first() {
                log::info!("Sleep wallpaper path: {}", path);
                if let Some(image) = self.load_sleep_wallpaper_from_path(ctx.source, path) {
                    log::info!("Sleep wallpaper loaded for {}", path);
                    self.render_wallpaper(ctx, &image);
                    if self.sleep_wallpaper_trbk_open {
                        ctx.source.close_trbk();
                        self.sleep_wallpaper_trbk_open = false;
                    }
                    self.sleep_from_home = false;
                    return;
                } else {
                    log::warn!("Sleep wallpaper load failed for {}", path);
                }
            }
        }
        self.sleep_from_home = false;
        self.render_sleep_fallback_logo(ctx);
        log::info!("Sleep wallpaper: none rendered");
    }

    fn render_sleep_fallback_logo<S: AppSource>(&mut self, ctx: &mut SystemRenderContext<'_, S>) {
        ctx.gray2_lsb.fill(0);
        ctx.gray2_msb.fill(0);
        let size = ctx.display_buffers.size();
        let x = ((size.width as i32) - ctx.logo.logo_w) / 2;
        let y = ((size.height as i32) - ctx.logo.logo_h) / 2;
        let mut gray2_used = false;
        draw_icon_gray2(
            ctx.display_buffers,
            ctx.gray2_lsb,
            ctx.gray2_msb,
            &mut gray2_used,
            x,
            y,
            ctx.logo.logo_w,
            ctx.logo.logo_h,
            ctx.logo.logo_dark,
            ctx.logo.logo_light,
        );
        if gray2_used {
            self.sleep_wallpaper_gray2 = true;
        }
    }

    fn load_sleep_wallpaper_from_path<S: AppSource>(
        &mut self,
        source: &mut S,
        path: &str,
    ) -> Option<ImageData> {
        let lower = path.to_ascii_lowercase();
        let mut parts: Vec<String> = path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| part.to_string())
            .collect();
        if parts.is_empty() {
            return None;
        }
        let file = parts.pop().unwrap_or_default();
        let entry = ImageEntry {
            name: file,
            kind: EntryKind::File,
        };
        if lower.ends_with(".trbk") {
            let info = source.open_trbk(&parts, &entry).ok()?;
            let image = if !info.images.is_empty() {
                source.trbk_image(0).ok()
            } else {
                None
            };
            if matches!(image, Some(ImageData::Gray2Stream { .. })) {
                self.sleep_wallpaper_trbk_open = true;
            } else {
                source.close_trbk();
            }
            return image;
        }
        if lower.ends_with(".tri") || lower.ends_with(".trimg") {
            return source.load(&parts, &entry).ok();
        }
        None
    }

    fn render_wallpaper<S: AppSource>(
        &mut self,
        ctx: &mut SystemRenderContext<'_, S>,
        image: &ImageData,
    ) {
        match image {
            ImageData::Gray2 { width, height, data } => {
                ctx.gray2_lsb.fill(0);
                ctx.gray2_msb.fill(0);
                let plane = (((*width as usize) * (*height as usize)) + 7) / 8;
                if data.len() >= plane * 3 {
                    Self::render_gray2_contain(
                        ctx.display_buffers,
                        ctx.display_buffers.rotation(),
                        ctx.gray2_lsb,
                        ctx.gray2_msb,
                        *width,
                        *height,
                        &data[..plane],
                        &data[plane..plane * 2],
                        &data[plane * 2..plane * 3],
                    );
                    self.sleep_wallpaper_gray2 = true;
                }
                return;
            }
            ImageData::Gray2Stream { width, height, key } => {
                ctx.gray2_lsb.fill(0);
                ctx.gray2_msb.fill(0);
                let target = ctx.display_buffers.size();
                let target_w = target.width as i32;
                let target_h = target.height as i32;
                let offset_x = ((target_w - *width as i32) / 2).max(0);
                let offset_y = ((target_h - *height as i32) / 2).max(0);
                if ctx
                    .source
                    .load_gray2_stream_region(
                        key,
                        *width,
                        *height,
                        ctx.display_buffers.rotation(),
                        ctx.display_buffers.get_active_buffer_mut(),
                        ctx.gray2_lsb,
                        ctx.gray2_msb,
                        offset_x,
                        offset_y,
                    )
                    .is_ok()
                {
                    self.sleep_wallpaper_gray2 = true;
                }
                return;
            }
            _ => {}
        }
        let size = ctx.display_buffers.size();
        let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
        let mut rq = RenderQueue::default();
        let mut ui = UiContext {
            buffers: ctx.display_buffers,
        };
        let mut reader = ReaderView::new(image);
        reader.refresh = RefreshMode::Full;
        reader.render(&mut ui, rect, &mut rq);
        let _ = rq;
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
                let Some((fx, fy)) = Self::map_display_point(rotation, dst_x, dst_y) else {
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

    fn save_rect_bits(buffers: &DisplayBuffers, rect: Rect) -> Vec<u8> {
        let mut out = Vec::with_capacity((rect.w * rect.h) as usize);
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                out.push(if Self::read_pixel(buffers, x, y) { 1 } else { 0 });
            }
        }
        out
    }

    fn read_pixel(buffers: &DisplayBuffers, x: i32, y: i32) -> bool {
        let size = buffers.size();
        if x < 0 || y < 0 || x as u32 >= size.width || y as u32 >= size.height {
            return true;
        }
        let (x, y) = match buffers.rotation() {
            Rotation::Rotate0 => (x as usize, y as usize),
            Rotation::Rotate90 => (y as usize, FB_HEIGHT - 1 - x as usize),
            Rotation::Rotate180 => (FB_WIDTH - 1 - x as usize, FB_HEIGHT - 1 - y as usize),
            Rotation::Rotate270 => (FB_WIDTH - 1 - y as usize, x as usize),
        };
        if x >= FB_WIDTH || y >= FB_HEIGHT {
            return true;
        }
        let index = y * FB_WIDTH + x;
        let byte_index = index / 8;
        let bit_index = 7 - (index % 8);
        let buffer = buffers.get_active_buffer();
        (buffer[byte_index] >> bit_index) & 0x01 == 1
    }
}
