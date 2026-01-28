extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_graphics::{
    Drawable,
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point, Primitive},
    text::Text,
};

use crate::{
    display::RefreshMode,
    framebuffer::{DisplayBuffers, Rotation, HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH},
    image_viewer::{EntryKind, ImageData, ImageEntry, ImageError, ImageSource},
    input,
    ui::{flush_queue, ListItem, ListView, ReaderView, Rect, RenderQueue, UiContext, View},
};

const LIST_TOP: i32 = 60;
const LINE_HEIGHT: i32 = 24;
const LIST_MARGIN_X: i32 = 16;
const HEADER_Y: i32 = 24;

pub struct Application<'a, S: ImageSource> {
    dirty: bool,
    display_buffers: &'a mut DisplayBuffers,
    source: &'a mut S,
    entries: Vec<ImageEntry>,
    selected: usize,
    state: AppState,
    current_image: Option<ImageData>,
    error_message: Option<String>,
    sleep_transition: bool,
    wake_transition: bool,
    full_refresh: bool,
    idle_ms: u32,
    idle_timeout_ms: u32,
    sleep_overlay: Option<SleepOverlay>,
    sleep_overlay_pending: bool,
    wake_restore_only: bool,
    resume_name: Option<String>,
    path: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AppState {
    Menu,
    Viewing,
    Sleeping,
    Error,
}

impl<'a, S: ImageSource> Application<'a, S> {
    pub fn new(display_buffers: &'a mut DisplayBuffers, source: &'a mut S) -> Self {
        display_buffers.set_rotation(Rotation::Rotate90);
        let resume_name = source.load_resume();
        let mut app = Application {
            dirty: true,
            display_buffers,
            source,
            entries: Vec::new(),
            selected: 0,
            state: AppState::Menu,
            current_image: None,
            error_message: None,
            sleep_transition: false,
            wake_transition: false,
            full_refresh: true,
            idle_ms: 0,
            idle_timeout_ms: 60_000,
            sleep_overlay: None,
            sleep_overlay_pending: false,
            wake_restore_only: false,
            resume_name,
            path: Vec::new(),
        };
        app.refresh_entries();
        app.try_resume();
        app
    }

    pub fn update(&mut self, buttons: &input::ButtonState, elapsed_ms: u32) {
        if self.state == AppState::Sleeping
            && (buttons.is_pressed(input::Buttons::Power)
                || buttons.is_held(input::Buttons::Power))
        {
            self.source.wake();
            let mut resumed_viewer = false;
            if let Some(overlay) = self.sleep_overlay.take() {
                self.restore_rect_bits(&overlay);
                self.state = AppState::Viewing;
                self.wake_restore_only = true;
                resumed_viewer = true;
            } else {
                self.state = AppState::Menu;
            }
            self.wake_transition = true;
            self.sleep_transition = false;
            self.full_refresh = true;
            self.dirty = true;
            self.idle_ms = 0;
            if !resumed_viewer {
                self.refresh_entries();
            }
            return;
        }

        if Self::has_input(buttons) {
            self.idle_ms = 0;
        }

        match self.state {
            AppState::Menu => {
                if buttons.is_pressed(input::Buttons::Up) {
                    if !self.entries.is_empty() {
                        self.selected = self.selected.saturating_sub(1);
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Down) {
                    if !self.entries.is_empty() {
                        self.selected = (self.selected + 1).min(self.entries.len() - 1);
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Confirm) {
                    self.open_selected();
                } else if buttons.is_pressed(input::Buttons::Back) {
                    if !self.path.is_empty() {
                        self.path.pop();
                        self.refresh_entries();
                    } else {
                        self.refresh_entries();
                    }
                }
            }
            AppState::Viewing => {
                if buttons.is_pressed(input::Buttons::Left) {
                    if !self.entries.is_empty() {
                        let next = self.selected.saturating_sub(1);
                        self.open_index(next);
                    }
                } else if buttons.is_pressed(input::Buttons::Right) {
                    if !self.entries.is_empty() {
                        let next = (self.selected + 1).min(self.entries.len() - 1);
                        self.open_index(next);
                    }
                } else if buttons.is_pressed(input::Buttons::Back)
                    || buttons.is_pressed(input::Buttons::Confirm)
                {
                    self.state = AppState::Menu;
                    self.dirty = true;
                    self.source.save_resume(None);
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        if let Some(name) = self.current_entry_name_owned() {
                            self.source.save_resume(Some(name.as_str()));
                        }
                        self.state = AppState::Sleeping;
                        self.sleep_transition = true;
                        self.sleep_overlay_pending = true;
                        self.dirty = true;
                    }
                }
            }
            AppState::Sleeping => {}
            AppState::Error => {
                if buttons.is_pressed(input::Buttons::Back)
                    || buttons.is_pressed(input::Buttons::Confirm)
                {
                    self.state = AppState::Menu;
                    self.error_message = None;
                    self.dirty = true;
                }
            }
        }
    }

    pub fn draw(&mut self, display: &mut impl crate::display::Display) {
        if !self.dirty {
            return;
        }

        self.dirty = false;
        match self.state {
            AppState::Menu => self.draw_menu(display),
            AppState::Viewing => self.draw_image(display),
            AppState::Sleeping => {
                if self.sleep_overlay_pending {
                    self.draw_sleep_overlay(display);
                    self.source.sleep();
                    self.sleep_overlay_pending = false;
                }
            }
            AppState::Error => self.draw_error(display),
        }
        self.full_refresh = false;
    }

    fn has_input(buttons: &input::ButtonState) -> bool {
        use input::Buttons::*;
        let list = [Back, Confirm, Left, Right, Up, Down, Power];
        list.iter()
            .any(|b| buttons.is_pressed(*b) || buttons.is_held(*b))
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

    fn open_selected(&mut self) {
        if self.entries.is_empty() {
            self.error_message = Some("No entries found in /images.".into());
            self.state = AppState::Error;
            self.dirty = true;
            return;
        }
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return;
        };
        match entry.kind {
            EntryKind::Dir => {
                self.path.push(entry.name);
                self.refresh_entries();
                if matches!(self.state, AppState::Error) {
                    self.path.pop();
                    self.refresh_entries();
                    self.set_error(ImageError::Message("Folder open failed.".into()));
                }
            }
            EntryKind::File => {
                if is_epub(&entry.name) {
                    self.set_error(ImageError::Message(
                        "EPUB support is not implemented yet.".into(),
                    ));
                    return;
                }
                match self.source.load(&self.path, &entry) {
                    Ok(image) => {
                        self.current_image = Some(image);
                        self.state = AppState::Viewing;
                        self.full_refresh = true;
                        self.dirty = true;
                        self.idle_ms = 0;
                        self.sleep_overlay = None;
                        self.sleep_overlay_pending = false;
                        if let Some(name) = self.current_entry_name_owned() {
                            self.source.save_resume(Some(name.as_str()));
                        }
                    }
                    Err(err) => self.set_error(err),
                }
            }
        }
    }

    fn open_index(&mut self, index: usize) {
        if self.entries.is_empty() {
            return;
        }
        let index = index.min(self.entries.len().saturating_sub(1));
        let Some(entry) = self.entries.get(index).cloned() else {
            return;
        };
        if entry.kind != EntryKind::File {
            return;
        }
        if is_epub(&entry.name) {
            self.set_error(ImageError::Message(
                "EPUB support is not implemented yet.".into(),
            ));
            return;
        }
        match self.source.load(&self.path, &entry) {
            Ok(image) => {
                self.selected = index;
                self.current_image = Some(image);
                self.state = AppState::Viewing;
                self.full_refresh = true;
                self.dirty = true;
                self.idle_ms = 0;
                self.sleep_overlay = None;
                self.sleep_overlay_pending = false;
                if let Some(name) = self.current_entry_name_owned() {
                    self.source.save_resume(Some(name.as_str()));
                }
            }
            Err(err) => self.set_error(err),
        }
    }

    fn refresh_entries(&mut self) {
        match self.source.refresh(&self.path) {
            Ok(entries) => {
                self.entries = entries;
                self.current_image = None;
                if self.selected >= self.entries.len() {
                    self.selected = 0;
                }
                self.state = AppState::Menu;
                self.error_message = None;
                self.dirty = true;
            }
            Err(err) => self.set_error(err),
        }
    }

    fn set_error(&mut self, err: ImageError) {
        let message = match err {
            ImageError::Io => "I/O error while accessing /images.".into(),
            ImageError::Decode => "Failed to decode image.".into(),
            ImageError::Unsupported => "Unsupported image format.".into(),
            ImageError::Message(message) => message,
        };
        self.error_message = Some(message);
        self.state = AppState::Error;
        self.dirty = true;
    }

    fn draw_menu(&mut self, display: &mut impl crate::display::Display) {
        let mut labels: Vec<String> = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            if entry.kind == EntryKind::Dir {
                let mut label = entry.name.clone();
                label.push('/');
                labels.push(label);
            } else {
                labels.push(entry.name.clone());
            }
        }
        let items: Vec<ListItem<'_>> = labels
            .iter()
            .map(|label| ListItem { label: label.as_str() })
            .collect();

        let title = self.menu_title();
        let mut list = ListView::new(&items);
        list.title = Some(title.as_str());
        list.footer = Some("Up/Down: select  Confirm: open  Back: up");
        list.empty_label = Some("No entries found in /images");
        list.selected = self.selected;
        list.margin_x = LIST_MARGIN_X;
        list.header_y = HEADER_Y;
        list.list_top = LIST_TOP;
        list.line_height = LINE_HEIGHT;

        let size = self.display_buffers.size();
        let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
        let mut rq = RenderQueue::default();
        let mut ctx = UiContext {
            buffers: self.display_buffers,
        };
        list.render(&mut ctx, rect, &mut rq);

        let fallback = if self.full_refresh {
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };
        flush_queue(display, self.display_buffers, &mut rq, fallback);
    }

    fn draw_error(&mut self, display: &mut impl crate::display::Display) {
        self.display_buffers.clear(BinaryColor::On).ok();
        let header_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new("Error", Point::new(LIST_MARGIN_X, HEADER_Y), header_style)
            .draw(self.display_buffers)
            .ok();
        if let Some(message) = &self.error_message {
            Text::new(message, Point::new(LIST_MARGIN_X, LIST_TOP), header_style)
                .draw(self.display_buffers)
                .ok();
        }
        Text::new(
            "Press Back to return",
            Point::new(LIST_MARGIN_X, LIST_TOP + 40),
            header_style,
        )
        .draw(self.display_buffers)
        .ok();
        let size = self.display_buffers.size();
        let mut rq = RenderQueue::default();
        rq.push(
            Rect::new(0, 0, size.width as i32, size.height as i32),
            RefreshMode::Full,
        );
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Full);
    }

    fn draw_image(&mut self, display: &mut impl crate::display::Display) {
        if self.wake_restore_only {
            self.wake_restore_only = false;
            let size = self.display_buffers.size();
            let mut rq = RenderQueue::default();
            rq.push(
                Rect::new(0, 0, size.width as i32, size.height as i32),
                RefreshMode::Fast,
            );
            flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
            return;
        }
        let Some(image) = self.current_image.take() else {
            self.set_error(ImageError::Decode);
            return;
        };
        let size = self.display_buffers.size();
        let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
        let mut rq = RenderQueue::default();
        let mut ctx = UiContext {
            buffers: self.display_buffers,
        };
        let mut reader = ReaderView::new(&image);
        reader.refresh = RefreshMode::Full;
        reader.render(&mut ctx, rect, &mut rq);
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Full);
        self.current_image = Some(image);
        // Sleep is handled via inactivity timeout.
    }

    fn draw_sleep_overlay(&mut self, display: &mut impl crate::display::Display) {
        let size = self.display_buffers.size();
        let text = "Sleeping...";
        let text_w = (text.len() as i32) * 10;
        let padding = 8;
        let bar_h = 28;
        let bar_w = (text_w + padding * 2).min(size.width as i32);
        let x = ((size.width as i32 - bar_w) / 2).max(0);
        let y = (size.height as i32 - bar_h).max(0);
        let rect = Rect::new(x, y, bar_w, bar_h);

        // Ensure we draw over the last displayed frame (active buffer may be stale post-swap).
        let inactive = *self.display_buffers.get_inactive_buffer();
        self.display_buffers
            .get_active_buffer_mut()
            .copy_from_slice(&inactive);

        let saved = self.save_rect_bits(rect);
        self.sleep_overlay = Some(SleepOverlay { rect, pixels: saved });

        embedded_graphics::primitives::Rectangle::new(
            embedded_graphics::prelude::Point::new(rect.x, rect.y),
            embedded_graphics::geometry::Size::new(rect.w as u32, rect.h as u32),
        )
        .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
            BinaryColor::Off,
        ))
        .draw(self.display_buffers)
        .ok();

        let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        let text_x = x + padding;
        let text_y = y + bar_h - 14;
        Text::new(text, Point::new(text_x, text_y), style)
            .draw(self.display_buffers)
            .ok();

        let mut rq = RenderQueue::default();
        rq.push(rect, RefreshMode::Fast);
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
    }

    fn save_rect_bits(&self, rect: Rect) -> Vec<u8> {
        let mut out = Vec::with_capacity((rect.w * rect.h) as usize);
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                out.push(if self.read_pixel(x, y) { 1 } else { 0 });
            }
        }
        out
    }

    fn restore_rect_bits(&mut self, overlay: &SleepOverlay) {
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
                self.display_buffers.set_pixel(xx, yy, color);
                idx += 1;
            }
        }
    }

    fn read_pixel(&self, x: i32, y: i32) -> bool {
        let size = self.display_buffers.size();
        if x < 0 || y < 0 || x as u32 >= size.width || y as u32 >= size.height {
            return true;
        }
        let (x, y) = match self.display_buffers.rotation() {
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
        let buffer = self.display_buffers.get_active_buffer();
        (buffer[byte_index] >> bit_index) & 0x01 == 1
    }

    fn try_resume(&mut self) {
        let Some(name) = self.resume_name.take() else {
            return;
        };
        let mut parts: Vec<String> = name
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| part.to_string())
            .collect();
        if parts.is_empty() {
            return;
        }
        let file = parts.pop().unwrap_or_default();
        self.path = parts;
        self.refresh_entries();
        let idx = self.entries.iter().position(|entry| entry.name == file);
        if let Some(index) = idx {
            self.open_index(index);
        } else {
            self.source.save_resume(None);
        }
    }

    fn current_entry_name_owned(&self) -> Option<String> {
        let entry = self.entries.get(self.selected)?;
        if entry.kind != EntryKind::File {
            return None;
        }
        let mut parts = self.path.clone();
        parts.push(entry.name.clone());
        Some(parts.join("/"))
    }

    fn menu_title(&self) -> String {
        if self.path.is_empty() {
            "Images".to_string()
        } else {
            let mut title = String::from("Images/");
            title.push_str(&self.path.join("/"));
            title
        }
    }
}

fn is_epub(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".epub")
}

struct SleepOverlay {
    rect: Rect,
    pixels: Vec<u8>,
}
