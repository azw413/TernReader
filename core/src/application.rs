extern crate alloc;

use alloc::{format, string::{String, ToString}};
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
const BOOK_FULL_REFRESH_EVERY: usize = 10;
const PAGE_INDICATOR_MARGIN: i32 = 12;
const PAGE_INDICATOR_Y: i32 = 24;

pub struct Application<'a, S: ImageSource> {
    dirty: bool,
    display_buffers: &'a mut DisplayBuffers,
    source: &'a mut S,
    entries: Vec<ImageEntry>,
    selected: usize,
    state: AppState,
    current_image: Option<ImageData>,
    current_book: Option<crate::trbk::TrbkBookInfo>,
    current_page_ops: Option<crate::trbk::TrbkPage>,
    toc_selected: usize,
    toc_labels: Option<Vec<String>>,
    current_page: usize,
    book_turns_since_full: usize,
    current_entry: Option<String>,
    last_viewed_entry: Option<String>,
    page_turn_indicator: Option<PageTurnIndicator>,
    last_rendered_page: Option<usize>,
    error_message: Option<String>,
    sleep_transition: bool,
    wake_transition: bool,
    full_refresh: bool,
    sleep_after_error: bool,
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
    BookViewing,
    Toc,
    SleepingPending,
    Sleeping,
    Error,
}

#[derive(Clone, Copy, Debug)]
enum PageTurnIndicator {
    Forward,
    Backward,
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
            current_book: None,
            current_page_ops: None,
            toc_selected: 0,
            toc_labels: None,
            current_page: 0,
            book_turns_since_full: 0,
            current_entry: None,
            last_viewed_entry: None,
            page_turn_indicator: None,
            last_rendered_page: None,
            error_message: None,
            sleep_transition: false,
            wake_transition: false,
            full_refresh: true,
            sleep_after_error: false,
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

        if self.state != AppState::Sleeping
            && self.state != AppState::SleepingPending
            && buttons.is_pressed(input::Buttons::Power)
        {
            self.start_sleep_request();
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
                        self.start_sleep_request();
                    }
                }
            }
            AppState::BookViewing => {
                if buttons.is_pressed(input::Buttons::Left)
                    || buttons.is_pressed(input::Buttons::Up)
                {
                    if self.current_page > 0 {
                        self.current_page = self.current_page.saturating_sub(1);
                        self.current_page_ops = None;
                        self.book_turns_since_full = self.book_turns_since_full.saturating_add(1);
                        self.page_turn_indicator = Some(PageTurnIndicator::Backward);
                        self.dirty = true;
                    }
                } else if buttons.is_pressed(input::Buttons::Right)
                    || buttons.is_pressed(input::Buttons::Down)
                {
                    if let Some(book) = &self.current_book {
                        if self.current_page + 1 < book.page_count {
                            self.current_page += 1;
                            self.current_page_ops = None;
                            self.book_turns_since_full = self.book_turns_since_full.saturating_add(1);
                            self.page_turn_indicator = Some(PageTurnIndicator::Forward);
                            self.dirty = true;
                        }
                    }
                } else if buttons.is_pressed(input::Buttons::Confirm) {
                    if let Some(book) = &self.current_book {
                        if !book.toc.is_empty() {
                            self.toc_selected = find_toc_selection(book, self.current_page);
                            self.toc_labels = None;
                            self.state = AppState::Toc;
                            self.dirty = true;
                        }
                    }
                } else if buttons.is_pressed(input::Buttons::Back) {
                    self.state = AppState::Menu;
                    self.current_book = None;
                    self.current_page_ops = None;
                    self.book_turns_since_full = 0;
                    self.source.close_trbk();
                    self.dirty = true;
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        self.start_sleep_request();
                    }
                }
            }
            AppState::Toc => {
                if let Some(book) = &self.current_book {
                    let toc_len = book.toc.len();
                    if buttons.is_pressed(input::Buttons::Up) {
                        if self.toc_selected > 0 {
                            self.toc_selected -= 1;
                            self.dirty = true;
                        }
                    } else if buttons.is_pressed(input::Buttons::Down) {
                        if self.toc_selected + 1 < toc_len {
                            self.toc_selected += 1;
                            self.dirty = true;
                        }
                    } else if buttons.is_pressed(input::Buttons::Confirm) {
                        if let Some(entry) = book.toc.get(self.toc_selected) {
                            self.current_page = entry.page_index as usize;
                            self.current_page_ops = None;
                            self.last_rendered_page = None;
                            self.state = AppState::BookViewing;
                            self.full_refresh = true;
                            self.book_turns_since_full = 0;
                            self.dirty = true;
                        }
                    } else if buttons.is_pressed(input::Buttons::Back) {
                        self.state = AppState::BookViewing;
                        self.dirty = true;
                    } else {
                        self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                        if self.idle_ms >= self.idle_timeout_ms {
                            self.start_sleep_request();
                        }
                    }
                } else {
                    self.state = AppState::BookViewing;
                    self.dirty = true;
                }
            }
            AppState::SleepingPending => {}
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
            AppState::BookViewing => {
                if let Some(indicator) = self.page_turn_indicator.take() {
                    self.draw_page_turn_indicator(display, indicator);
                }
                self.draw_book(display);
            }
            AppState::Toc => self.draw_toc(display),
            AppState::SleepingPending => {
                self.draw_sleeping_indicator(display);
                if self.save_resume_checked() {
                    self.state = AppState::Sleeping;
                    self.sleep_transition = true;
                    self.sleep_overlay_pending = true;
                    self.draw_sleep_overlay(display);
                    self.source.sleep();
                    self.sleep_overlay_pending = false;
                }
            }
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
        if self.state == AppState::Error && self.sleep_after_error {
            self.sleep_after_error = false;
            self.state = AppState::Sleeping;
            self.sleep_transition = true;
            self.sleep_overlay_pending = true;
            self.dirty = true;
        }
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
                if is_trbk(&entry.name) {
                    match self.source.open_trbk(&self.path, &entry) {
                        Ok(info) => {
                            let entry_name = self.entry_path_string(&entry);
                            self.current_entry = Some(entry_name.clone());
                            self.last_viewed_entry = Some(entry_name);
                            log::info!("Opened book entry: {:?}", self.current_entry);
                            self.current_book = Some(info);
                            self.toc_labels = None;
                            self.current_page = 0;
                            self.current_page_ops = self.source.trbk_page(0).ok();
                            self.last_rendered_page = None;
                            self.state = AppState::BookViewing;
                            self.full_refresh = true;
                            self.book_turns_since_full = 0;
                            self.dirty = true;
                        }
                        Err(err) => self.set_error(err),
                    }
                    return;
                }
                if is_epub(&entry.name) {
                    self.set_error(ImageError::Message(
                        "EPUB files must be converted to .trbk.".into(),
                    ));
                    return;
                }
                match self.source.load(&self.path, &entry) {
                    Ok(image) => {
                        let entry_name = self.entry_path_string(&entry);
                        self.current_entry = Some(entry_name.clone());
                        self.last_viewed_entry = Some(entry_name);
                        log::info!("Opened image entry: {:?}", self.current_entry);
                        self.current_image = Some(image);
                        self.state = AppState::Viewing;
                        self.full_refresh = true;
                        self.dirty = true;
                        self.idle_ms = 0;
                        self.sleep_overlay = None;
                        self.sleep_overlay_pending = false;
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
        if is_trbk(&entry.name) {
            match self.source.open_trbk(&self.path, &entry) {
                Ok(info) => {
                    let entry_name = self.entry_path_string(&entry);
                    self.current_entry = Some(entry_name.clone());
                    self.last_viewed_entry = Some(entry_name);
                    log::info!("Opened book entry: {:?}", self.current_entry);
                    self.current_book = Some(info);
                    self.toc_labels = None;
                    self.current_page = 0;
                    self.current_page_ops = self.source.trbk_page(0).ok();
                    self.last_rendered_page = None;
                    self.state = AppState::BookViewing;
                    self.full_refresh = true;
                    self.book_turns_since_full = 0;
                    self.dirty = true;
                }
                Err(err) => self.set_error(err),
            }
            return;
        }
        if is_epub(&entry.name) {
            self.set_error(ImageError::Message(
                "EPUB files must be converted to .trbk.".into(),
            ));
            return;
        }
        match self.source.load(&self.path, &entry) {
            Ok(image) => {
                self.selected = index;
                let entry_name = self.entry_path_string(&entry);
                self.current_entry = Some(entry_name.clone());
                self.last_viewed_entry = Some(entry_name);
                log::info!("Opened image entry: {:?}", self.current_entry);
                self.current_image = Some(image);
                self.state = AppState::Viewing;
                self.full_refresh = true;
                self.dirty = true;
                self.idle_ms = 0;
                self.sleep_overlay = None;
                self.sleep_overlay_pending = false;
            }
            Err(err) => self.set_error(err),
        }
    }

    fn refresh_entries(&mut self) {
        match self.source.refresh(&self.path) {
            Ok(entries) => {
                self.entries = entries;
                self.current_image = None;
                self.current_book = None;
                self.current_page_ops = None;
                self.current_page = 0;
                self.toc_labels = None;
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

    fn draw_toc(&mut self, display: &mut impl crate::display::Display) {
        self.display_buffers.clear(BinaryColor::On).ok();
        let Some(book) = &self.current_book else {
            self.set_error(ImageError::Decode);
            return;
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

        let size = self.display_buffers.size();
        let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
        let mut rq = RenderQueue::default();
        let mut ctx = UiContext {
            buffers: self.display_buffers,
        };
        list.render(&mut ctx, rect, &mut rq);
        let refresh = if self.full_refresh {
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };
        flush_queue(display, self.display_buffers, &mut rq, refresh);
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

    fn draw_book(&mut self, display: &mut impl crate::display::Display) {
        self.display_buffers.clear(BinaryColor::On).ok();
        let Some(book) = &self.current_book else {
            self.set_error(ImageError::Decode);
            return;
        };
        if self.current_page_ops.is_none() {
            self.current_page_ops = self.source.trbk_page(self.current_page).ok();
        }
        if let Some(page) = self.current_page_ops.as_ref() {
            for op in &page.ops {
                match op {
                    crate::trbk::TrbkOp::TextRun { x, y, style, text } => {
                        Self::draw_trbk_text(self.display_buffers, book, *x, *y, *style, text);
                    }
                    crate::trbk::TrbkOp::Image {
                        x,
                        y,
                        width,
                        height,
                        image_index,
                    } => {
                        if let Ok(image) = self.source.trbk_image(*image_index as usize) {
                            Self::draw_trbk_image(
                                self.display_buffers,
                                &image,
                                *x,
                                *y,
                                *width as i32,
                                *height as i32,
                            );
                        }
                    }
                }
            }
        }
        self.last_rendered_page = Some(self.current_page);
        Self::draw_page_indicator(self.display_buffers, self.current_page, book.page_count);
        if self.book_turns_since_full >= BOOK_FULL_REFRESH_EVERY {
            self.full_refresh = true;
            self.book_turns_since_full = 0;
        }
        let mode = if self.full_refresh {
            RefreshMode::Full
        } else {
            RefreshMode::Fast
        };
        let mut rq = RenderQueue::default();
        let size = self.display_buffers.size();
        rq.push(
            Rect::new(0, 0, size.width as i32, size.height as i32),
            mode,
        );
        flush_queue(display, self.display_buffers, &mut rq, mode);
    }

    fn draw_trbk_text(
        buffers: &mut DisplayBuffers,
        book: &crate::trbk::TrbkBookInfo,
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
            if let Some(glyph) = find_glyph(&book.glyphs, style, codepoint) {
                draw_glyph(buffers, glyph, pen_x, baseline);
                pen_x += glyph.x_advance as i32;
            } else {
                pen_x += book.metadata.char_width as i32;
            }
        }
    }

    fn draw_trbk_image(
        buffers: &mut DisplayBuffers,
        image: &ImageData,
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

    fn draw_page_turn_indicator(
        &mut self,
        display: &mut impl crate::display::Display,
        indicator: PageTurnIndicator,
    ) {
        let size = self.display_buffers.size();
        // Ensure we draw over the last displayed frame (active buffer may be stale).
        let inactive = *self.display_buffers.get_inactive_buffer();
        self.display_buffers
            .get_active_buffer_mut()
            .copy_from_slice(&inactive);
        let symbol = match indicator {
            PageTurnIndicator::Forward => ">",
            PageTurnIndicator::Backward => "<",
        };
        let text_w = (symbol.len() as i32) * 10;
        let x = match indicator {
            PageTurnIndicator::Forward => (size.width as i32 - PAGE_INDICATOR_MARGIN - text_w)
                .max(PAGE_INDICATOR_MARGIN),
            PageTurnIndicator::Backward => PAGE_INDICATOR_MARGIN,
        };
        let y = PAGE_INDICATOR_Y;
        let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new(symbol, Point::new(x, y), style)
            .draw(self.display_buffers)
            .ok();
        Text::new(symbol, Point::new(x + 1, y), style)
            .draw(self.display_buffers)
            .ok();

        let mut rq = RenderQueue::default();
        rq.push(Rect::new(x - 2, y - 2, text_w + 4, 22), RefreshMode::Fast);
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
    }

    fn draw_sleeping_indicator(&mut self, display: &mut impl crate::display::Display) {
        let size = self.display_buffers.size();
        // Ensure we draw over the last displayed frame.
        let inactive = *self.display_buffers.get_inactive_buffer();
        self.display_buffers
            .get_active_buffer_mut()
            .copy_from_slice(&inactive);

        let text = "Zz";
        let text_w = (text.len() as i32) * 10;
        let x = (size.width as i32 - PAGE_INDICATOR_MARGIN - text_w)
            .max(PAGE_INDICATOR_MARGIN);
        let y = PAGE_INDICATOR_Y;
        let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new(text, Point::new(x, y), style)
            .draw(self.display_buffers)
            .ok();
        Text::new(text, Point::new(x + 1, y), style)
            .draw(self.display_buffers)
            .ok();

        let mut rq = RenderQueue::default();
        rq.push(Rect::new(x - 2, y - 2, text_w + 4, 22), RefreshMode::Fast);
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
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
        let Some(raw) = self.resume_name.take() else {
            return;
        };
        let (name, resume_page) = if let Some((path, page)) = raw.rsplit_once('\t') {
            let page = page.parse::<usize>().ok();
            (path.to_string(), page)
        } else {
            (raw, None)
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
            if let Some(page) = resume_page {
                if let Some(book) = &self.current_book {
                    if page < book.page_count {
                        self.current_page = page;
                        self.current_page_ops = self.source.trbk_page(self.current_page).ok();
                        self.full_refresh = true;
                        self.book_turns_since_full = 0;
                        self.dirty = true;
                    }
                }
            }
        } else {
            self.source.save_resume(None);
        }
    }

    fn current_entry_name_owned(&self) -> Option<String> {
        let entry = self.entries.get(self.selected)?;
        if entry.kind != EntryKind::File {
            return None;
        }
        Some(self.entry_path_string(entry))
    }

    fn entry_path_string(&self, entry: &ImageEntry) -> String {
        let mut parts = self.path.clone();
        parts.push(entry.name.clone());
        parts.join("/")
    }

    fn current_resume_string(&self) -> Option<String> {
        let name = self
            .current_entry
            .clone()
            .or_else(|| self.last_viewed_entry.clone())
            .or_else(|| self.current_entry_name_owned())?;
        let page = self
            .last_rendered_page
            .unwrap_or(self.current_page);
        if self.current_book.is_some() {
            return Some(format!("{}\t{}", name, page));
        }
        Some(name)
    }

    fn save_resume_checked(&mut self) -> bool {
        let resume_debug = format!(
            "state={:?} current_entry={:?} last_viewed_entry={:?} path={:?} selected={} has_book={} current_page={} last_rendered={:?}",
            self.state,
            self.current_entry,
            self.last_viewed_entry,
            self.path,
            self.selected,
            self.current_book.is_some(),
            self.current_page,
            self.last_rendered_page
        );
        let Some(expected) = self.current_resume_string() else {
            log::info!("No resume state to save. {}", resume_debug);
            return true;
        };
        log::info!("Saving resume state: {} ({})", expected, resume_debug);
        self.source.save_resume(Some(expected.as_str()));
        let actual = self.source.load_resume().unwrap_or_default();
        log::info!("Resume state readback: {}", actual);
        if actual.is_empty() || actual != expected {
            self.error_message = Some("Failed to save resume state.".into());
            self.state = AppState::Error;
            self.sleep_after_error = true;
            self.dirty = true;
            return false;
        }
        true
    }

    fn start_sleep_request(&mut self) {
        if self.state == AppState::Sleeping || self.state == AppState::SleepingPending {
            return;
        }
        self.state = AppState::SleepingPending;
        self.sleep_transition = false;
        self.sleep_overlay_pending = false;
        self.dirty = true;
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

fn find_glyph<'a>(
    glyphs: &'a [crate::trbk::TrbkGlyph],
    style: u8,
    codepoint: u32,
) -> Option<&'a crate::trbk::TrbkGlyph> {
    glyphs
        .iter()
        .find(|glyph| glyph.style == style && glyph.codepoint == codepoint)
}

fn find_toc_selection(book: &crate::trbk::TrbkBookInfo, page: usize) -> usize {
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
    let mut idx = 0usize;
    for row in 0..height {
        for col in 0..width {
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            if byte < glyph.bitmap.len() && (glyph.bitmap[byte] & (1 << bit)) != 0 {
                buffers.set_pixel(start_x + col, start_y + row, BinaryColor::Off);
            }
            idx += 1;
        }
    }
}

fn is_epub(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".epub") || name.ends_with(".epb")
}

fn is_trbk(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".trbk")
}

struct SleepOverlay {
    rect: Rect,
    pixels: Vec<u8>,
}
