extern crate alloc;

use alloc::{format, string::{String, ToString}};
use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::BTreeMap;

use embedded_graphics::{
    Drawable,
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point, Primitive, Size},
    primitives::Rectangle,
    text::Text,
};

mod generated_icons {
    include!(concat!(env!("OUT_DIR"), "/icons.rs"));
}

use crate::{
    display::{GrayscaleMode, RefreshMode},
    framebuffer::{DisplayBuffers, Rotation, HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH},
    image_viewer::{EntryKind, ImageData, ImageEntry, ImageError, ImageSource},
    input,
    ui::{flush_queue, ListItem, ListView, ReaderView, Rect, RenderQueue, UiContext, View},
};

fn basename_from_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

const LIST_TOP: i32 = 60;
const LINE_HEIGHT: i32 = 24;
const LIST_MARGIN_X: i32 = 16;
const HEADER_Y: i32 = 24;
const BOOK_FULL_REFRESH_EVERY: usize = 10;
const PAGE_INDICATOR_MARGIN: i32 = 12;
const PAGE_INDICATOR_Y: i32 = 24;
const START_MENU_MARGIN: i32 = 16;
const START_MENU_RECENT_THUMB: i32 = 44;
const START_MENU_ACTION_GAP: i32 = 12;
const DEBUG_GRAY2_MODE: u8 = 0; // 0=normal, 1=base, 2=lsb, 3=msb

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
    book_positions: BTreeMap<String, usize>,
    recent_entries: Vec<String>,
    path: Vec<String>,
    gray2_lsb: Vec<u8>,
    gray2_msb: Vec<u8>,
    start_menu_section: StartMenuSection,
    start_menu_index: usize,
    start_menu_cache: Vec<RecentPreview>,
    sleep_from_home: bool,
    recent_dirty: bool,
    book_positions_dirty: bool,
    last_saved_resume: Option<String>,
    exit_from: ExitFrom,
    exit_overlay_drawn: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AppState {
    StartMenu,
    Menu,
    Viewing,
    BookViewing,
    ExitingPending,
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

#[derive(Clone, Copy, Debug)]
enum ExitFrom {
    Image,
    Book,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartMenuSection {
    Recents,
    Actions,
}

#[derive(Clone, Copy, Debug)]
enum StartMenuAction {
    FileBrowser,
    Settings,
    Battery,
}

struct RecentPreview {
    path: String,
    title: String,
    image: Option<ImageData>,
}

impl<'a, S: ImageSource> Application<'a, S> {
    pub fn new(display_buffers: &'a mut DisplayBuffers, source: &'a mut S) -> Self {
        display_buffers.set_rotation(Rotation::Rotate90);
        let resume_name = source.load_resume();
        let book_positions = source
            .load_book_positions()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let recent_entries = source.load_recent_entries();
        let mut app = Application {
            dirty: true,
            display_buffers,
            source,
            entries: Vec::new(),
            selected: 0,
            state: AppState::StartMenu,
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
            idle_timeout_ms: 300_000,
            sleep_overlay: None,
            sleep_overlay_pending: false,
            wake_restore_only: false,
            resume_name,
            book_positions,
            recent_entries,
            path: Vec::new(),
            gray2_lsb: vec![0u8; crate::framebuffer::BUFFER_SIZE],
            gray2_msb: vec![0u8; crate::framebuffer::BUFFER_SIZE],
            start_menu_section: StartMenuSection::Recents,
            start_menu_index: 0,
            start_menu_cache: Vec::new(),
            sleep_from_home: false,
            recent_dirty: false,
            book_positions_dirty: false,
            last_saved_resume: None,
            exit_from: ExitFrom::Image,
            exit_overlay_drawn: false,
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
                self.state = AppState::StartMenu;
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
            AppState::StartMenu => {
                let recents = self.collect_recent_paths();
                let recent_len = recents.len();
                if buttons.is_pressed(input::Buttons::Up) {
                    match self.start_menu_section {
                        StartMenuSection::Recents => {
                            if self.start_menu_index > 0 {
                                self.start_menu_index -= 1;
                            }
                        }
                        StartMenuSection::Actions => {
                            if recent_len > 0 {
                                self.start_menu_section = StartMenuSection::Recents;
                                self.start_menu_index = recent_len.saturating_sub(1);
                            }
                        }
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Down) {
                    match self.start_menu_section {
                        StartMenuSection::Recents => {
                            if self.start_menu_index + 1 < recent_len {
                                self.start_menu_index += 1;
                            } else {
                                self.start_menu_section = StartMenuSection::Actions;
                                self.start_menu_index = 0;
                            }
                        }
                        StartMenuSection::Actions => {
                            if self.start_menu_index + 1 < 3 {
                                self.start_menu_index += 1;
                            }
                        }
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Left) {
                    if self.start_menu_section == StartMenuSection::Actions {
                        self.start_menu_index = self.start_menu_index.saturating_sub(1);
                        self.dirty = true;
                    }
                } else if buttons.is_pressed(input::Buttons::Right) {
                    if self.start_menu_section == StartMenuSection::Actions {
                        self.start_menu_index = (self.start_menu_index + 1).min(2);
                        self.dirty = true;
                    }
                } else if buttons.is_pressed(input::Buttons::Confirm) {
                    match self.start_menu_section {
                        StartMenuSection::Recents => {
                            if let Some(path) = recents.get(self.start_menu_index) {
                                self.open_recent_path(path);
                            }
                        }
                        StartMenuSection::Actions => {
                            match self.start_menu_index {
                                0 => {
                                    self.state = AppState::Menu;
                                    self.selected = 0;
                                    self.refresh_entries();
                                    self.dirty = true;
                                }
                                1 => {
                                    self.set_error(ImageError::Message(
                                        "Settings not implemented yet.".into(),
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        self.start_sleep_request();
                    }
                }
            }
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
                        self.state = AppState::StartMenu;
                        self.dirty = true;
                    }
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        self.start_sleep_request();
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
                    self.exit_from = ExitFrom::Image;
                    self.exit_overlay_drawn = false;
                    self.state = AppState::ExitingPending;
                    self.dirty = true;
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
                    self.exit_from = ExitFrom::Book;
                    self.exit_overlay_drawn = false;
                    self.state = AppState::ExitingPending;
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
            AppState::ExitingPending => {}
            AppState::Error => {
                if buttons.is_pressed(input::Buttons::Back)
                    || buttons.is_pressed(input::Buttons::Confirm)
                {
                    self.state = AppState::StartMenu;
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
            AppState::StartMenu => self.draw_start_menu(display),
            AppState::Menu => self.draw_menu(display),
            AppState::Viewing => self.draw_image(display),
            AppState::BookViewing => {
                if let Some(indicator) = self.page_turn_indicator.take() {
                    self.draw_page_turn_indicator(display, indicator);
                }
                self.draw_book(display);
            }
            AppState::ExitingPending => {
                if !self.exit_overlay_drawn {
                    match self.exit_from {
                        ExitFrom::Image => self.draw_image(display),
                        ExitFrom::Book => self.draw_book(display),
                    }
                    self.draw_exiting_overlay(display);
                    self.exit_overlay_drawn = true;
                    self.dirty = true;
                    return;
                }
                match self.exit_from {
                    ExitFrom::Image => {
                        self.source.save_resume(None);
                        self.save_recent_entries_now();
                    }
                    ExitFrom::Book => {
                        self.update_book_position();
                        self.save_book_positions_now();
                        self.save_recent_entries_now();
                        self.current_book = None;
                        self.current_page_ops = None;
                        self.book_turns_since_full = 0;
                        self.source.close_trbk();
                    }
                }
                self.state = AppState::StartMenu;
                self.start_menu_cache.clear();
                self.dirty = true;
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
                        self.last_viewed_entry = Some(entry_name.clone());
                        self.mark_recent(entry_name);
                        log::info!("Opened book entry: {:?}", self.current_entry);
                            self.current_book = Some(info);
                            self.toc_labels = None;
                            self.current_page = self
                                .current_entry
                                .as_ref()
                                .and_then(|name| self.book_positions.get(name).copied())
                                .unwrap_or(0);
                            self.current_page_ops = self.source.trbk_page(self.current_page).ok();
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
                        self.last_viewed_entry = Some(entry_name.clone());
                        self.mark_recent(entry_name);
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
                    self.last_viewed_entry = Some(entry_name.clone());
                    self.mark_recent(entry_name);
                    log::info!("Opened book entry: {:?}", self.current_entry);
                    self.current_book = Some(info);
                    self.toc_labels = None;
                    self.current_page = self
                        .current_entry
                        .as_ref()
                        .and_then(|name| self.book_positions.get(name).copied())
                        .unwrap_or(0);
                    self.current_page_ops = self.source.trbk_page(self.current_page).ok();
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
                self.last_viewed_entry = Some(entry_name.clone());
                self.mark_recent(entry_name);
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
                if self.state != AppState::StartMenu {
                    self.state = AppState::Menu;
                }
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

    fn draw_start_menu(&mut self, display: &mut impl crate::display::Display) {
        self.display_buffers.clear(BinaryColor::On).ok();
        let mut gray2_used = false;
        self.gray2_lsb.fill(0);
        self.gray2_msb.fill(0);
        let size = self.display_buffers.size();
        let width = size.width as i32;
        let height = size.height as i32;
        let mid_y = (height * 82) / 100;

        let header_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new("Recents", Point::new(START_MENU_MARGIN, HEADER_Y), header_style)
            .draw(self.display_buffers)
            .ok();

        let recents = self.collect_recent_paths();
        self.ensure_start_menu_cache(&recents);
        let list_top = HEADER_Y + 24;
        let max_items = 6usize;
        let list_width = width - (START_MENU_MARGIN * 2);
        let item_height = 99;
        let thumb_size = 74;
        let mut draw_count = 0usize;
        for (idx, preview) in self.start_menu_cache.iter().take(max_items).enumerate() {
            let y = list_top + (idx as i32 * item_height);
            if y + item_height > mid_y {
                break;
            }
            let is_selected = self.start_menu_section == StartMenuSection::Recents
                && self.start_menu_index == idx;
            if is_selected {
                Rectangle::new(
                    Point::new(START_MENU_MARGIN - 4, y - 4),
                    Size::new((list_width + 8) as u32, (item_height - 4) as u32),
                )
                .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
                    BinaryColor::Off,
                ))
                .draw(self.display_buffers)
                .ok();
            }
            let thumb_x = START_MENU_MARGIN;
        let thumb_y = y + (item_height - thumb_size) / 2 - 2;
            Rectangle::new(
                Point::new(thumb_x, thumb_y),
                Size::new(thumb_size as u32, thumb_size as u32),
            )
            .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_stroke(
                if is_selected {
                    BinaryColor::On
                } else {
                    BinaryColor::Off
                },
                1,
            ))
            .draw(self.display_buffers)
            .ok();
            if let Some(image) = preview.image.as_ref() {
                let mut gray2_ctx = Some((
                    self.gray2_lsb.as_mut_slice(),
                    self.gray2_msb.as_mut_slice(),
                    &mut gray2_used,
                ));
                Self::draw_trbk_image(
                    self.display_buffers,
                    &image,
                    &mut gray2_ctx,
                    thumb_x + 2,
                    thumb_y + 2,
                    thumb_size - 4,
                    thumb_size - 4,
                );
            }
            let text_color = if is_selected {
                BinaryColor::On
            } else {
                BinaryColor::Off
            };
            let label_style = MonoTextStyle::new(&FONT_10X20, text_color);
            Text::new(
                &preview.title,
                Point::new(thumb_x + thumb_size + 12, y + 26),
                label_style,
            )
            .draw(self.display_buffers)
            .ok();
            draw_count += 1;
        }
        if draw_count == 0 {
            Text::new(
                "No recent items.",
                Point::new(START_MENU_MARGIN, list_top + 24),
                header_style,
            )
            .draw(self.display_buffers)
            .ok();
        }

        Rectangle::new(
            Point::new(START_MENU_MARGIN, mid_y),
            Size::new((width - (START_MENU_MARGIN * 2)) as u32, 1),
        )
        .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
            BinaryColor::Off,
        ))
        .draw(self.display_buffers)
        .ok();

        let action_top = mid_y + 17;
        let action_width = (width - (START_MENU_MARGIN * 2) - (START_MENU_ACTION_GAP * 2)) / 3;
        let action_height = 110;
        let actions = [
            (StartMenuAction::FileBrowser, "Files"),
            (StartMenuAction::Settings, "Settings"),
            (StartMenuAction::Battery, "Battery"),
        ];
        for (idx, (_, label)) in actions.iter().enumerate() {
            let x = START_MENU_MARGIN + idx as i32 * (action_width + START_MENU_ACTION_GAP);
            let y = action_top;
            let is_selected = self.start_menu_section == StartMenuSection::Actions
                && self.start_menu_index == idx;
            if is_selected {
                Rectangle::new(
                    Point::new(x - 4, y - 4),
                    Size::new((action_width + 8) as u32, (action_height + 8) as u32),
                )
                .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
                    BinaryColor::Off,
                ))
                .draw(self.display_buffers)
                .ok();
            }
            Rectangle::new(
                Point::new(x, y),
                Size::new(action_width as u32, action_height as u32),
            )
            .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_stroke(
                if is_selected {
                    BinaryColor::On
                } else {
                    BinaryColor::Off
                },
                1,
            ))
            .draw(self.display_buffers)
            .ok();
            let icon_color = if is_selected {
                BinaryColor::On
            } else {
                BinaryColor::Off
            };
            let icon_size = generated_icons::ICON_SIZE as i32;
            let icon_x = x + (action_width - icon_size) / 2;
            let icon_y = y + 5;
            match idx {
                0 => Self::draw_icon_mask(
                    self.display_buffers,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    generated_icons::ICON_FOLDER_MASK,
                    icon_color,
                ),
                1 => Self::draw_icon_mask(
                    self.display_buffers,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    generated_icons::ICON_GEAR_MASK,
                    icon_color,
                ),
                _ => Self::draw_icon_mask(
                    self.display_buffers,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    generated_icons::ICON_BATTERY_MASK,
                    icon_color,
                ),
            }
            let text_color = if is_selected {
                BinaryColor::On
            } else {
                BinaryColor::Off
            };
            let label_style = MonoTextStyle::new(&FONT_10X20, text_color);
            let label_width = (label.len() as i32) * 10;
            let label_x = x + (action_width - label_width) / 2;
            Text::new(
                label,
                Point::new(label_x, y + action_height - 12),
                label_style,
            )
            .draw(self.display_buffers)
            .ok();
            if *label == "Battery" {
                Text::new("--%", Point::new(label_x, y + action_height - 34), label_style)
                    .draw(self.display_buffers)
                    .ok();
            }
        }

        let mut rq = RenderQueue::default();
        rq.push(
            Rect::new(0, 0, width, height),
            if self.full_refresh {
                RefreshMode::Full
            } else {
                RefreshMode::Fast
            },
        );
        flush_queue(
            display,
            self.display_buffers,
            &mut rq,
            if self.full_refresh {
                RefreshMode::Full
            } else {
                RefreshMode::Fast
            },
        );
        if gray2_used {
            let lsb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                self.gray2_lsb.as_slice().try_into().unwrap();
            let msb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                self.gray2_msb.as_slice().try_into().unwrap();
            display.copy_grayscale_buffers(lsb_buf, msb_buf);
            display.display_differential_grayscale(false);
        }
    }

    fn draw_exiting_overlay(&mut self, display: &mut impl crate::display::Display) {
        let size = self.display_buffers.size();
        let width = size.width as i32;
        let text = "Exiting...";
        let text_width = (text.len() as i32) * 10;
        let padding_x = 10;
        let padding_y = 6;
        let rect_w = text_width + (padding_x * 2);
        let rect_h = 20 + (padding_y * 2);
        let x = (width - rect_w) / 2;
        let y = 6;
        Rectangle::new(
            Point::new(x, y),
            Size::new(rect_w as u32, rect_h as u32),
        )
        .into_styled(embedded_graphics::primitives::PrimitiveStyle::with_fill(
            BinaryColor::Off,
        ))
        .draw(self.display_buffers)
        .ok();
        let text_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
        Text::new(text, Point::new(x + padding_x, y + 20), text_style)
            .draw(self.display_buffers)
            .ok();

        let mut rq = RenderQueue::default();
        rq.push(
            Rect::new(x, y, rect_w, rect_h),
            RefreshMode::Fast,
        );
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
    }

    fn draw_icon_mask(
        buffers: &mut DisplayBuffers,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        mask: &[u8],
        color: BinaryColor,
    ) {
        if width <= 0 || height <= 0 {
            return;
        }
        let width_u = width as usize;
        let height_u = height as usize;
        let expected = (width_u * height_u + 7) / 8;
        if mask.len() != expected {
            return;
        }
        for yy in 0..height_u {
            for xx in 0..width_u {
                let idx = yy * width_u + xx;
                let byte = mask[idx / 8];
                let bit = 7 - (idx % 8);
                if (byte >> bit) & 1 == 1 {
                    buffers.set_pixel(x + xx as i32, y + yy as i32, color);
                }
            }
        }
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
        match &image {
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
                self.display_buffers.clear(BinaryColor::On).ok();
                self.gray2_lsb.fill(0);
                self.gray2_msb.fill(0);
                Self::render_gray2_contain(
                    self.display_buffers,
                    self.display_buffers.rotation(),
                    &mut self.gray2_lsb,
                    &mut self.gray2_msb,
                    *width,
                    *height,
                    base,
                    lsb,
                    msb,
                );
                self.display_buffers.copy_active_to_inactive();
                if DEBUG_GRAY2_MODE != 0 {
                    self.apply_gray2_debug_overlay(DEBUG_GRAY2_MODE);
                    display.display(self.display_buffers, RefreshMode::Full);
                } else {
                    let lsb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                        self.gray2_lsb.as_slice().try_into().unwrap();
                    let msb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                        self.gray2_msb.as_slice().try_into().unwrap();
                    display.copy_grayscale_buffers(lsb_buf, msb_buf);
                    display.display_absolute_grayscale(GrayscaleMode::Fast);
                }
            }
            ImageData::Gray2Stream { width, height, key } => {
                let plane = ((*width as usize * *height as usize) + 7) / 8;
                if plane > crate::framebuffer::BUFFER_SIZE {
                    self.set_error(ImageError::Message(
                        "Image size not supported on device.".into(),
                    ));
                    return;
                }
                let rotation = self.display_buffers.rotation();
                let size = self.display_buffers.size();
                if *width != size.width || *height != size.height {
                    self.set_error(ImageError::Message(
                        "Grayscale images must match display size.".into(),
                    ));
                    return;
                }
                let base_buf = self.display_buffers.get_active_buffer_mut();
                base_buf.fill(0xFF);
                self.gray2_lsb.fill(0);
                self.gray2_msb.fill(0);
                if self
                    .source
                    .load_gray2_stream(
                        key,
                        *width,
                        *height,
                        rotation,
                        base_buf,
                        &mut self.gray2_lsb,
                        &mut self.gray2_msb,
                    )
                    .is_err()
                {
                    self.set_error(ImageError::Decode);
                    return;
                }
                self.display_buffers.copy_active_to_inactive();
                if DEBUG_GRAY2_MODE != 0 {
                    self.apply_gray2_debug_overlay(DEBUG_GRAY2_MODE);
                    display.display(self.display_buffers, RefreshMode::Full);
                } else {
                    let lsb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                        self.gray2_lsb.as_slice().try_into().unwrap();
                    let msb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                        self.gray2_msb.as_slice().try_into().unwrap();
                    display.copy_grayscale_buffers(lsb_buf, msb_buf);
                    display.display_absolute_grayscale(GrayscaleMode::Fast);
                }
            }
            _ => {
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
            }
        }
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
        let mut gray2_used = false;
        let mut gray2_absolute = false;
        self.gray2_lsb.fill(0);
        self.gray2_msb.fill(0);
        if let Some(page) = self.current_page_ops.as_ref() {
            for op in &page.ops {
                match op {
                    crate::trbk::TrbkOp::TextRun { x, y, style, text } => {
                        let mut gray2_ctx = Some((
                            self.gray2_lsb.as_mut_slice(),
                            self.gray2_msb.as_mut_slice(),
                            &mut gray2_used,
                        ));
                        Self::draw_trbk_text(
                            self.display_buffers,
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
                        if let Ok(image) = self.source.trbk_image(*image_index as usize) {
                            let mut gray2_ctx = Some((
                                self.gray2_lsb.as_mut_slice(),
                                self.gray2_msb.as_mut_slice(),
                                &mut gray2_used,
                            ));
                            match &image {
                                ImageData::Gray2Stream { width, height, key } => {
                                    let size = self.display_buffers.size();
                                    if *x == 0
                                        && *y == 0
                                        && op_w == size.width
                                        && op_h == size.height
                                        && *width == op_w
                                        && *height == op_h
                                    {
                                        let rotation = self.display_buffers.rotation();
                                        let base_buf =
                                            self.display_buffers.get_active_buffer_mut();
                                        base_buf.fill(0xFF);
                                        if self
                                            .source
                                            .load_gray2_stream(
                                                key,
                                                *width,
                                                *height,
                                                rotation,
                                                base_buf,
                                                self.gray2_lsb.as_mut_slice(),
                                                self.gray2_msb.as_mut_slice(),
                                            )
                                            .is_ok()
                                        {
                                            gray2_used = true;
                                            gray2_absolute = true;
                                        }
                                    }
                                }
                                _ => {
                                    Self::draw_trbk_image(
                                        self.display_buffers,
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
        if gray2_used {
            display.display(self.display_buffers, mode);
            let lsb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                self.gray2_lsb.as_slice().try_into().unwrap();
            let msb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                self.gray2_msb.as_slice().try_into().unwrap();
            display.copy_grayscale_buffers(lsb_buf, msb_buf);
            if gray2_absolute {
                display.display_absolute_grayscale(GrayscaleMode::Fast);
            } else {
                display.display_differential_grayscale(false);
            }
        } else {
            let mut rq = RenderQueue::default();
            let size = self.display_buffers.size();
            rq.push(
                Rect::new(0, 0, size.width as i32, size.height as i32),
                mode,
            );
            flush_queue(display, self.display_buffers, &mut rq, mode);
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

    fn draw_trbk_image(
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
                            Self::map_display_point(buffers.rotation(), dst_x, dst_y)
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

    fn apply_gray2_debug_overlay(&mut self, mode: u8) {
        if mode == 0 {
            return;
        }
        let active = self.display_buffers.get_active_buffer_mut();
        match mode {
            1 => {}
            2 => {
                for (dst, src) in active.iter_mut().zip(self.gray2_lsb.iter()) {
                    *dst = !*src;
                }
            }
            3 => {
                for (dst, src) in active.iter_mut().zip(self.gray2_msb.iter()) {
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

        self.display_buffers.clear(BinaryColor::On).ok();
        self.draw_sleep_wallpaper();

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
        rq.push(
            Rect::new(0, 0, size.width as i32, size.height as i32),
            RefreshMode::Full,
        );
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Full);
    }

    fn draw_sleep_wallpaper(&mut self) {
        if self.current_image.is_some() {
            if let Some(image) = self.current_image.take() {
                self.render_wallpaper(&image);
                self.current_image = Some(image);
            }
            return;
        }
        if self.current_book.is_some() {
            if let Ok(image) = self.source.trbk_image(0) {
                self.render_wallpaper(&image);
            }
            return;
        }
        if self.state == AppState::StartMenu {
            let recents = self.collect_recent_paths();
            if let Some(path) = recents.first() {
                if let Some(image) = self.load_sleep_wallpaper_from_path(path) {
                    self.render_wallpaper(&image);
                }
            }
        }
    }

    fn load_sleep_wallpaper_from_path(&mut self, path: &str) -> Option<ImageData> {
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
            let info = self.source.open_trbk(&parts, &entry).ok()?;
            let image = if !info.images.is_empty() {
                self.source.trbk_image(0).ok()
            } else {
                None
            };
            self.source.close_trbk();
            return image;
        }
        if lower.ends_with(".tri") || lower.ends_with(".trimg") {
            return self.source.load(&parts, &entry).ok();
        }
        None
    }

    fn render_wallpaper(&mut self, image: &ImageData) {
        let size = self.display_buffers.size();
        let rect = Rect::new(0, 0, size.width as i32, size.height as i32);
        let mut rq = RenderQueue::default();
        let mut ctx = UiContext {
            buffers: self.display_buffers,
        };
        let mut reader = ReaderView::new(image);
        reader.refresh = RefreshMode::Full;
        reader.render(&mut ctx, rect, &mut rq);
        let _ = rq;
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
        let name = raw;
        if name == "HOME" {
            return;
        }
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
            if let Some(book) = &self.current_book {
                if let Some(name) = &self.current_entry {
                    if let Some(page) = self.book_positions.get(name).copied() {
                        if page < book.page_count {
                            self.current_page = page;
                            self.current_page_ops = self.source.trbk_page(self.current_page).ok();
                            self.full_refresh = true;
                            self.book_turns_since_full = 0;
                            self.dirty = true;
                        }
                    }
                }
            }
        } else {
            self.source.save_resume(None);
        }
    }

    fn collect_recent_paths(&self) -> Vec<String> {
        let mut recent = self.recent_entries.clone();
        if let Some(entry) = &self.last_viewed_entry {
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

    fn open_recent_path(&mut self, path: &str) {
        let mut parts: Vec<String> = path
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
            self.selected = index;
            self.open_index(index);
        } else {
            self.set_error(ImageError::Message("Recent entry not found.".into()));
        }
    }

    fn ensure_start_menu_cache(&mut self, recents: &[String]) {
        let same = recents.len() == self.start_menu_cache.len()
            && recents
                .iter()
                .zip(self.start_menu_cache.iter())
                .all(|(path, cached)| path == &cached.path);
        if same {
            return;
        }
        self.start_menu_cache.clear();
        for path in recents {
            let (title, image) = self.load_recent_preview(path);
            self.start_menu_cache.push(RecentPreview {
                path: path.clone(),
                title,
                image,
            });
        }
    }

    fn load_recent_preview(&mut self, path: &str) -> (String, Option<ImageData>) {
        let label_fallback = basename_from_path(path);
        if let Some(image) = self.source.load_thumbnail(path) {
            let title = self
                .source
                .load_thumbnail_title(path)
                .filter(|value| !value.is_empty())
                .unwrap_or(label_fallback);
            return (title, Some(image));
        }
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".tri") || lower.ends_with(".trimg") {
            let mut parts: Vec<String> = path
                .split('/')
                .filter(|part| !part.is_empty())
                .map(|part| part.to_string())
                .collect();
            if parts.is_empty() {
                return (label_fallback, None);
            }
            let file = parts.pop().unwrap_or_default();
            let entry = ImageEntry {
                name: file,
                kind: EntryKind::File,
            };
            if let Ok(image) = self.source.load(&parts, &entry) {
                if let Some(thumb) = self.thumbnail_from_image(&image, 74) {
                    self.source.save_thumbnail(path, &thumb);
                    return (label_fallback, Some(thumb));
                }
            }
            return (label_fallback, None);
        }
        if !lower.ends_with(".trbk") {
            return (label_fallback, None);
        }
        let mut parts: Vec<String> = path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| part.to_string())
            .collect();
        if parts.is_empty() {
            return (label_fallback, None);
        }
        let file = parts.pop().unwrap_or_default();
        let entry = ImageEntry {
            name: file,
            kind: EntryKind::File,
        };
        let info = match self.source.open_trbk(&parts, &entry) {
            Ok(info) => info,
            Err(_) => {
                self.source.close_trbk();
                return (label_fallback, None);
            }
        };
        let title = if info.metadata.title.is_empty() {
            label_fallback
        } else {
            info.metadata.title.clone()
        };
        let preview = if !info.images.is_empty() {
            self.source.trbk_image(0).ok().and_then(|image| {
                self.thumbnail_from_image(&image, START_MENU_RECENT_THUMB as u32)
            })
        } else {
            None
        };
        self.source.close_trbk();
        if let Some(image) = preview.as_ref() {
            self.source.save_thumbnail(path, image);
            self.source.save_thumbnail_title(path, &title);
        }
        (title, preview)
    }

    fn thumbnail_from_image(&self, image: &ImageData, size: u32) -> Option<ImageData> {
        let (src_w, src_h) = match image {
            ImageData::Mono1 { width, height, .. } => (*width, *height),
            ImageData::Gray8 { width, height, .. } => (*width, *height),
            ImageData::Gray2 { width, height, .. } => (*width, *height),
            ImageData::Gray2Stream { width, height, .. } => (*width, *height),
        };
        if src_w == 0 || src_h == 0 {
            return None;
        }
        let dst_w = size;
        let dst_h = size;
        let dst_len = ((dst_w as usize * dst_h as usize) + 7) / 8;
        let mut base = Vec::new();
        let mut lsb = Vec::new();
        let mut msb = Vec::new();
        base.resize(dst_len, 0xFF);
        lsb.resize(dst_len, 0x00);
        msb.resize(dst_len, 0x00);
        for y in 0..dst_h {
            for x in 0..dst_w {
                let sx = (x * src_w) / dst_w;
                let sy = (y * src_h) / dst_h;
                let lum = match image {
                    ImageData::Mono1 { width, bits, .. } => {
                        let idx = (sy * (*width) + sx) as usize;
                        let byte = bits[idx / 8];
                        let bit = 7 - (idx % 8);
                        if (byte >> bit) & 1 == 1 { 255 } else { 0 }
                    }
                    ImageData::Gray8 { width, pixels, .. } => {
                        let idx = (sy * (*width) + sx) as usize;
                        pixels.get(idx).copied().unwrap_or(255)
                    }
                    ImageData::Gray2 {
                        width,
                        height,
                        data,
                        ..
                    } => {
                        let idx = (sy * (*width) + sx) as usize;
                        let byte = idx / 8;
                        let bit = 7 - (idx % 8);
                        let plane_len = (((*width) as usize * (*height) as usize) + 7) / 8;
                        if data.len() < plane_len * 3 {
                            255
                        } else {
                            let bw = (data[byte] >> bit) & 1;
                            let l = (data[plane_len + byte] >> bit) & 1;
                            let m = (data[plane_len * 2 + byte] >> bit) & 1;
                            match (m, l, bw) {
                                (0, 0, 1) => 255,
                                (0, 1, 1) => 192,
                                (1, 0, 0) => 128,
                                (1, 1, 0) => 64,
                                _ => 0,
                            }
                        }
                    }
                    ImageData::Gray2Stream { .. } => 255,
                };
                let dst_idx = (y * dst_w + x) as usize;
                let dst_byte = dst_idx / 8;
                let dst_bit = 7 - (dst_idx % 8);
                let (bw_bit, msb_bit, lsb_bit) = if lum >= 205 {
                    (1u8, 0u8, 0u8)
                } else if lum >= 154 {
                    (1u8, 0u8, 1u8)
                } else if lum >= 103 {
                    (0u8, 1u8, 0u8)
                } else if lum >= 52 {
                    (0u8, 1u8, 1u8)
                } else {
                    (0u8, 1u8, 1u8)
                };
                if bw_bit != 0 {
                    base[dst_byte] |= 1 << dst_bit;
                } else {
                    base[dst_byte] &= !(1 << dst_bit);
                }
                if lsb_bit != 0 {
                    lsb[dst_byte] |= 1 << dst_bit;
                }
                if msb_bit != 0 {
                    msb[dst_byte] |= 1 << dst_bit;
                }
            }
        }
        let mut data = Vec::with_capacity(dst_len * 3);
        data.extend_from_slice(&base);
        data.extend_from_slice(&lsb);
        data.extend_from_slice(&msb);
        Some(ImageData::Gray2 {
            width: dst_w,
            height: dst_h,
            data,
        })
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
        if self.state == AppState::StartMenu {
            return Some("HOME".to_string());
        }
        let name = self
            .current_entry
            .clone()
            .or_else(|| self.last_viewed_entry.clone())
            .or_else(|| self.current_entry_name_owned())?;
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
        let expected = if self.sleep_from_home {
            Some("HOME".to_string())
        } else {
            self.current_resume_string()
        };
        let Some(expected) = expected else {
            log::info!("No resume state to save. {}", resume_debug);
            return true;
        };
        log::info!("Saving resume state: {} ({})", expected, resume_debug);
        self.update_book_position();
        self.save_book_positions_now();
        self.save_recent_entries_now();
        if self.last_saved_resume.as_deref() != Some(expected.as_str()) {
            self.source.save_resume(Some(expected.as_str()));
            let actual = self.source.load_resume().unwrap_or_default();
            log::info!("Resume state readback: {}", actual);
            self.last_saved_resume = Some(actual.clone());
            if actual.is_empty() || actual != expected {
                self.error_message = Some("Failed to save resume state.".into());
                self.state = AppState::Error;
                self.sleep_after_error = true;
                self.dirty = true;
                self.sleep_from_home = false;
                return false;
            }
        }
        self.sleep_from_home = false;
        true
    }

    fn update_book_position(&mut self) {
        if self.current_book.is_some() {
            if let Some(name) = self
                .current_entry
                .clone()
                .or_else(|| self.last_viewed_entry.clone())
            {
                let prev = self.book_positions.insert(name, self.current_page);
                if prev != Some(self.current_page) {
                    self.book_positions_dirty = true;
                }
            }
        }
    }

    fn mark_recent(&mut self, path: String) {
        self.recent_entries.retain(|entry| entry != &path);
        self.recent_entries.insert(0, path);
        if self.recent_entries.len() > 10 {
            self.recent_entries.truncate(10);
        }
        self.recent_dirty = true;
    }

    fn save_book_positions_now(&mut self) {
        if !self.book_positions_dirty {
            return;
        }
        let entries: Vec<(String, usize)> = self
            .book_positions
            .iter()
            .map(|(name, page)| (name.clone(), *page))
            .collect();
        self.source.save_book_positions(&entries);
        self.book_positions_dirty = false;
    }

    fn save_recent_entries_now(&mut self) {
        if !self.recent_dirty {
            return;
        }
        self.source.save_recent_entries(&self.recent_entries);
        self.recent_dirty = false;
    }

    fn start_sleep_request(&mut self) {
        if self.state == AppState::Sleeping || self.state == AppState::SleepingPending {
            return;
        }
        self.sleep_from_home = self.state == AppState::StartMenu;
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
