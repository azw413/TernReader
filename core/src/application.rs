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

fn is_trbk(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".trbk")
}

fn is_epub(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".epub") || name.ends_with(".epb")
}

use crate::{
    app::{
        book_reader::{draw_trbk_image, BookReaderContext, BookReaderState, PageTurnIndicator},
        home::{HomeState, RecentPreview, StartMenuAction, StartMenuSection},
        image_viewer::{ImageViewerContext, ImageViewerState},
    },
    build_info,
    display::{GrayscaleMode, RefreshMode},
    framebuffer::{DisplayBuffers, Rotation, BUFFER_SIZE, HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH},
    image_viewer::{AppSource, EntryKind, ImageData, ImageEntry, ImageError},
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
const PAGE_INDICATOR_MARGIN: i32 = 12;
const PAGE_INDICATOR_Y: i32 = 24;
const START_MENU_MARGIN: i32 = 16;
const START_MENU_RECENT_THUMB: i32 = 74;
const START_MENU_ACTION_GAP: i32 = 12;
pub struct Application<'a, S: AppSource> {
    dirty: bool,
    display_buffers: &'a mut DisplayBuffers,
    source: &'a mut S,
    home: HomeState,
    state: AppState,
    image_viewer: ImageViewerState,
    book_reader: BookReaderState,
    current_entry: Option<String>,
    last_viewed_entry: Option<String>,
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
    gray2_lsb: Vec<u8>,
    gray2_msb: Vec<u8>,
    sleep_from_home: bool,
    sleep_wallpaper_gray2: bool,
    sleep_wallpaper_trbk_open: bool,
    recent_dirty: bool,
    book_positions_dirty: bool,
    last_saved_resume: Option<String>,
    exit_from: ExitFrom,
    exit_overlay_drawn: bool,
    battery_percent: Option<u8>,
}

struct SleepOverlay {
    rect: Rect,
    pixels: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AppState {
    StartMenu,
    Settings,
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
enum ExitFrom {
    Image,
    Book,
}

impl<'a, S: AppSource> Application<'a, S> {
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
            home: HomeState::new(),
            state: AppState::StartMenu,
            image_viewer: ImageViewerState::new(),
            book_reader: BookReaderState::new(),
            current_entry: None,
            last_viewed_entry: None,
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
            gray2_lsb: vec![0u8; crate::framebuffer::BUFFER_SIZE],
            gray2_msb: vec![0u8; crate::framebuffer::BUFFER_SIZE],
            sleep_from_home: false,
            sleep_wallpaper_gray2: false,
            sleep_wallpaper_trbk_open: false,
            recent_dirty: false,
            book_positions_dirty: false,
            last_saved_resume: None,
            exit_from: ExitFrom::Image,
            exit_overlay_drawn: false,
            battery_percent: None,
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
                self.home.start_menu_need_base_refresh = true;
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
                    self.home.start_menu_prev_section = self.home.start_menu_section;
                    self.home.start_menu_prev_index = self.home.start_menu_index;
                    match self.home.start_menu_section {
                        StartMenuSection::Recents => {
                            if self.home.start_menu_index > 0 {
                                self.home.start_menu_index -= 1;
                            }
                        }
                        StartMenuSection::Actions => {
                            if recent_len > 0 {
                                self.home.start_menu_section = StartMenuSection::Recents;
                                self.home.start_menu_index = recent_len.saturating_sub(1);
                            }
                        }
                    }
                    self.home.start_menu_nav_pending = true;
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Down) {
                    self.home.start_menu_prev_section = self.home.start_menu_section;
                    self.home.start_menu_prev_index = self.home.start_menu_index;
                    match self.home.start_menu_section {
                        StartMenuSection::Recents => {
                            if self.home.start_menu_index + 1 < recent_len {
                                self.home.start_menu_index += 1;
                            } else {
                                self.home.start_menu_section = StartMenuSection::Actions;
                                self.home.start_menu_index = 0;
                            }
                        }
                        StartMenuSection::Actions => {
                            if self.home.start_menu_index + 1 < 3 {
                                self.home.start_menu_index += 1;
                            }
                        }
                    }
                    self.home.start_menu_nav_pending = true;
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Left) {
                    if self.home.start_menu_section == StartMenuSection::Actions {
                        self.home.start_menu_prev_section = self.home.start_menu_section;
                        self.home.start_menu_prev_index = self.home.start_menu_index;
                        self.home.start_menu_index = self.home.start_menu_index.saturating_sub(1);
                        self.home.start_menu_nav_pending = true;
                        self.dirty = true;
                    }
                } else if buttons.is_pressed(input::Buttons::Right) {
                    if self.home.start_menu_section == StartMenuSection::Actions {
                        self.home.start_menu_prev_section = self.home.start_menu_section;
                        self.home.start_menu_prev_index = self.home.start_menu_index;
                        self.home.start_menu_index = (self.home.start_menu_index + 1).min(2);
                        self.home.start_menu_nav_pending = true;
                        self.dirty = true;
                    }
                } else if buttons.is_pressed(input::Buttons::Confirm) {
                    match self.home.start_menu_section {
                        StartMenuSection::Recents => {
                            if let Some(path) = recents.get(self.home.start_menu_index) {
                                self.open_recent_path(path);
                            }
                        }
                        StartMenuSection::Actions => {
                            match self.home.start_menu_index {
                                0 => {
                                    self.state = AppState::Menu;
                                    self.home.selected = 0;
                                    self.refresh_entries();
                                    self.dirty = true;
                                }
                                1 => {
                                    self.state = AppState::Settings;
                                    self.dirty = true;
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
                    if !self.home.entries.is_empty() {
                        self.home.selected = self.home.selected.saturating_sub(1);
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Down) {
                    if !self.home.entries.is_empty() {
                        self.home.selected = (self.home.selected + 1).min(self.home.entries.len() - 1);
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Confirm) {
                    self.open_selected();
                } else if buttons.is_pressed(input::Buttons::Back) {
                    if !self.home.path.is_empty() {
                        self.home.path.pop();
                        self.refresh_entries();
                    } else {
                        self.state = AppState::StartMenu;
                        self.home.start_menu_need_base_refresh = true;
                        self.dirty = true;
                    }
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        self.start_sleep_request();
                    }
                }
            }
            AppState::Settings => {
                if buttons.is_pressed(input::Buttons::Back)
                    || buttons.is_pressed(input::Buttons::Confirm)
                {
                    self.state = AppState::StartMenu;
                    self.home.start_menu_need_base_refresh = true;
                    self.dirty = true;
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        self.start_sleep_request();
                    }
                }
            }
            AppState::Viewing => {
                if buttons.is_pressed(input::Buttons::Left) {
                    if !self.home.entries.is_empty() {
                        let next = self.home.selected.saturating_sub(1);
                        self.open_index(next);
                    }
                } else if buttons.is_pressed(input::Buttons::Right) {
                    if !self.home.entries.is_empty() {
                        let next = (self.home.selected + 1).min(self.home.entries.len() - 1);
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
                let result = self
                    .book_reader
                    .handle_view_input(self.source, buttons);
                if result.exit {
                    self.exit_from = ExitFrom::Book;
                    self.exit_overlay_drawn = false;
                    self.state = AppState::ExitingPending;
                    self.dirty = true;
                } else if result.open_toc {
                    self.state = AppState::Toc;
                    self.dirty = true;
                } else if result.dirty {
                    self.dirty = true;
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        self.start_sleep_request();
                    }
                }
            }
            AppState::Toc => {
                let result = self.book_reader.handle_toc_input(buttons);
                if result.exit {
                    self.state = AppState::BookViewing;
                    self.dirty = true;
                } else if result.jumped {
                    self.state = AppState::BookViewing;
                    self.full_refresh = true;
                    self.dirty = true;
                } else if result.dirty {
                    self.dirty = true;
                } else {
                    self.idle_ms = self.idle_ms.saturating_add(elapsed_ms);
                    if self.idle_ms >= self.idle_timeout_ms {
                        self.start_sleep_request();
                    }
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
                    self.home.start_menu_need_base_refresh = true;
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
            AppState::Settings => self.draw_settings(display),
            AppState::Menu => self.draw_menu(display),
            AppState::Viewing => self.draw_image_viewer(display),
            AppState::BookViewing => {
                if let Some(indicator) = self.book_reader.take_page_turn_indicator() {
                    self.draw_page_turn_indicator(display, indicator);
                }
                self.draw_book_reader(display);
            }
            AppState::ExitingPending => {
                if !self.exit_overlay_drawn {
                    match self.exit_from {
                        ExitFrom::Image => self.draw_image_viewer(display),
                        ExitFrom::Book => self.draw_book_reader(display),
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
                        self.book_reader.close(self.source);
                    }
                }
                self.state = AppState::StartMenu;
                self.home.start_menu_cache.clear();
                self.home.start_menu_need_base_refresh = true;
                self.dirty = true;
            }
            AppState::Toc => self.draw_toc_view(display),
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

    pub fn set_battery_percent(&mut self, percent: Option<u8>) {
        if self.battery_percent == percent {
            return;
        }
        self.battery_percent = percent;
        if self.state == AppState::StartMenu {
            self.dirty = true;
        }
    }

    fn open_selected(&mut self) {
        if self.home.entries.is_empty() {
            self.error_message = Some("No entries found in /images.".into());
            self.state = AppState::Error;
            self.dirty = true;
            return;
        }
        let Some(entry) = self.home.entries.get(self.home.selected).cloned() else {
            return;
        };
        match entry.kind {
            EntryKind::Dir => {
                self.home.path.push(entry.name);
                self.refresh_entries();
                if matches!(self.state, AppState::Error) {
                    self.home.path.pop();
                    self.refresh_entries();
                    self.set_error(ImageError::Message("Folder open failed.".into()));
                }
            }
            EntryKind::File => {
                if is_trbk(&entry.name) {
                    let entry_name = self.home.entry_path_string(&entry);
                    match self.book_reader.open(
                        self.source,
                        &self.home.path,
                        &entry,
                        &entry_name,
                        &self.book_positions,
                    ) {
                        Ok(()) => {
                            self.current_entry = Some(entry_name.clone());
                            self.last_viewed_entry = Some(entry_name.clone());
                            self.mark_recent(entry_name);
                            log::info!("Opened book entry: {:?}", self.current_entry);
                            self.state = AppState::BookViewing;
                            self.full_refresh = true;
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
                match self.image_viewer.open(self.source, &self.home.path, &entry) {
                    Ok(()) => {
                        let entry_name = self.home.entry_path_string(&entry);
                        self.current_entry = Some(entry_name.clone());
                        self.last_viewed_entry = Some(entry_name.clone());
                        self.mark_recent(entry_name);
                        log::info!("Opened image entry: {:?}", self.current_entry);
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
        if self.home.entries.is_empty() {
            return;
        }
        let index = index.min(self.home.entries.len().saturating_sub(1));
        let Some(entry) = self.home.entries.get(index).cloned() else {
            return;
        };
        if entry.kind != EntryKind::File {
            return;
        }
        if is_trbk(&entry.name) {
            let entry_name = self.home.entry_path_string(&entry);
            match self.book_reader.open(
                self.source,
                &self.home.path,
                &entry,
                &entry_name,
                &self.book_positions,
            ) {
                Ok(()) => {
                    self.current_entry = Some(entry_name.clone());
                    self.last_viewed_entry = Some(entry_name.clone());
                    self.mark_recent(entry_name);
                    log::info!("Opened book entry: {:?}", self.current_entry);
                    self.state = AppState::BookViewing;
                    self.full_refresh = true;
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
        match self.image_viewer.open(self.source, &self.home.path, &entry) {
            Ok(()) => {
                self.home.selected = index;
                let entry_name = self.home.entry_path_string(&entry);
                self.current_entry = Some(entry_name.clone());
                self.last_viewed_entry = Some(entry_name.clone());
                self.mark_recent(entry_name);
                log::info!("Opened image entry: {:?}", self.current_entry);
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
        match self.source.refresh(&self.home.path) {
            Ok(entries) => {
                self.home.entries = entries;
                self.image_viewer.clear();
                self.book_reader.clear();
                if self.home.selected >= self.home.entries.len() {
                    self.home.selected = 0;
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
        let size = self.display_buffers.size();
        let width = size.width as i32;
        let height = size.height as i32;
        let mid_y = (height * 82) / 100;

        let recents = self.collect_recent_paths();
        self.ensure_start_menu_cache(&recents);

        let list_top = HEADER_Y + 24;
        let max_items = 6usize;
        let list_width = width - (START_MENU_MARGIN * 2);
        let item_height = 99;
        let thumb_size = 74;
        let action_top = mid_y + 17;
        let action_width = (width - (START_MENU_MARGIN * 2) - (START_MENU_ACTION_GAP * 2)) / 3;
        let action_height = 110;

        if self.home.start_menu_need_base_refresh {
            let (gray2_used, draw_count) = self.render_start_menu_contents(
                true,
                width,
                mid_y,
                list_top,
                max_items,
                list_width,
                item_height,
                thumb_size,
                action_top,
                action_width,
                action_height,
            );
            log::info!(
                "Start menu base render: recents={}, cache={}",
                draw_count,
                self.home.start_menu_cache.len()
            );
            if gray2_used {
                self.merge_bw_into_gray2();
                let lsb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                    self.gray2_lsb.as_slice().try_into().unwrap();
                let msb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                    self.gray2_msb.as_slice().try_into().unwrap();
                display.copy_grayscale_buffers(lsb_buf, msb_buf);
                display.display_absolute_grayscale(GrayscaleMode::Fast);
            } else {
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
            }
            self.home.start_menu_need_base_refresh = false;
            self.render_start_menu_contents(
                false,
                width,
                mid_y,
                list_top,
                max_items,
                list_width,
                item_height,
                thumb_size,
                action_top,
                action_width,
                action_height,
            );
            let rect_for = |section: StartMenuSection, index: usize| -> Option<Rect> {
                match section {
                    StartMenuSection::Recents => {
                        if index >= max_items {
                            return None;
                        }
                        let y = list_top + (index as i32 * item_height);
                        if y + item_height > mid_y {
                            return None;
                        }
                        Some(Rect::new(
                            START_MENU_MARGIN - 4,
                            y - 4,
                            list_width + 8,
                            item_height - 4,
                        ))
                    }
                    StartMenuSection::Actions => {
                        if index >= 3 {
                            return None;
                        }
                        let x = START_MENU_MARGIN
                            + index as i32 * (action_width + START_MENU_ACTION_GAP);
                        Some(Rect::new(
                            x - 4,
                            action_top - 4,
                            action_width + 8,
                            action_height + 8,
                        ))
                    }
                }
            };
            if let Some(rect) = rect_for(self.home.start_menu_section, self.home.start_menu_index) {
                let mut rq = RenderQueue::default();
                rq.push(rect, RefreshMode::Fast);
                flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
            }
            return;
        }

        let (gray2_used, draw_count) = self.render_start_menu_contents(
            false,
            width,
            mid_y,
            list_top,
            max_items,
            list_width,
            item_height,
            thumb_size,
            action_top,
            action_width,
            action_height,
        );
        log::info!(
            "Start menu render: recents={}, cache={}",
            draw_count,
            self.home.start_menu_cache.len()
        );
        if gray2_used {
            if self.home.start_menu_nav_pending {
                let mut rq = RenderQueue::default();
                let mut push_rect = |rect: Rect| {
                    rq.push(rect, RefreshMode::Fast);
                };
                let rect_for = |section: StartMenuSection, index: usize| -> Option<Rect> {
                    match section {
                        StartMenuSection::Recents => {
                            if index >= max_items {
                                return None;
                            }
                            let y = list_top + (index as i32 * item_height);
                            if y + item_height > mid_y {
                                return None;
                            }
                            Some(Rect::new(
                                START_MENU_MARGIN - 4,
                                y - 4,
                                list_width + 8,
                                item_height - 4,
                            ))
                        }
                        StartMenuSection::Actions => {
                            if index >= 3 {
                                return None;
                            }
                            let x = START_MENU_MARGIN
                                + index as i32 * (action_width + START_MENU_ACTION_GAP);
                            Some(Rect::new(
                                x - 4,
                                action_top - 4,
                                action_width + 8,
                                action_height + 8,
                            ))
                        }
                    }
                };
                if let Some(rect) =
                    rect_for(self.home.start_menu_prev_section, self.home.start_menu_prev_index)
                {
                    push_rect(rect);
                }
                if (self.home.start_menu_prev_section != self.home.start_menu_section)
                    || (self.home.start_menu_prev_index != self.home.start_menu_index)
                {
                    if let Some(rect) = rect_for(self.home.start_menu_section, self.home.start_menu_index) {
                        push_rect(rect);
                    }
                }
                flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
                self.home.start_menu_nav_pending = false;
            } else {
                self.merge_bw_into_gray2();
                let lsb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                    self.gray2_lsb.as_slice().try_into().unwrap();
                let msb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                    self.gray2_msb.as_slice().try_into().unwrap();
                display.copy_grayscale_buffers(lsb_buf, msb_buf);
                display.display_absolute_grayscale(GrayscaleMode::Fast);
            }
        } else {
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

    fn render_start_menu_contents(
        &mut self,
        suppress_selection: bool,
        width: i32,
        mid_y: i32,
        list_top: i32,
        max_items: usize,
        list_width: i32,
        item_height: i32,
        thumb_size: i32,
        action_top: i32,
        action_width: i32,
        action_height: i32,
    ) -> (bool, usize) {
        let header_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        self.display_buffers.clear(BinaryColor::On).ok();
        self.gray2_lsb.fill(0);
        self.gray2_msb.fill(0);
        let mut gray2_used = false;
        self.gray2_lsb.fill(0);
        self.gray2_msb.fill(0);

        Text::new("Recents", Point::new(START_MENU_MARGIN, HEADER_Y), header_style)
            .draw(self.display_buffers)
            .ok();

        let mut draw_count = 0usize;
        for (idx, preview) in self.home.start_menu_cache.iter().take(max_items).enumerate() {
            let y = list_top + (idx as i32 * item_height);
            if y + item_height > mid_y {
                break;
            }
            let is_selected = !suppress_selection
                && self.home.start_menu_section == StartMenuSection::Recents
                && self.home.start_menu_index == idx;
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
                if let Some(mono) = self.thumbnail_to_mono(image) {
                    let mut gray2_ctx = None;
                    draw_trbk_image(
                        self.display_buffers,
                        &mono,
                        &mut gray2_ctx,
                        thumb_x + 2,
                        thumb_y + 2,
                        thumb_size - 4,
                        thumb_size - 4,
                    );
                } else {
                    let mut gray2_ctx = Some((
                        self.gray2_lsb.as_mut_slice(),
                        self.gray2_msb.as_mut_slice(),
                        &mut gray2_used,
                    ));
                    draw_trbk_image(
                        self.display_buffers,
                        &image,
                        &mut gray2_ctx,
                        thumb_x + 2,
                        thumb_y + 2,
                        thumb_size - 4,
                        thumb_size - 4,
                    );
                }
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

        let actions = [
            (StartMenuAction::FileBrowser, "Files"),
            (StartMenuAction::Settings, "Settings"),
            (StartMenuAction::Battery, ""),
        ];
        for (idx, (_, label)) in actions.iter().enumerate() {
            let x = START_MENU_MARGIN + idx as i32 * (action_width + START_MENU_ACTION_GAP);
            let y = action_top;
            let is_selected = !suppress_selection
                && self.home.start_menu_section == StartMenuSection::Actions
                && self.home.start_menu_index == idx;
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
            let icon_size = generated_icons::ICON_SIZE as i32;
            let icon_x = x + (action_width - icon_size) / 2;
            let icon_y = y + 5;
            match idx {
                0 => Self::draw_icon_gray2(
                    self.display_buffers,
                    self.gray2_lsb.as_mut_slice(),
                    self.gray2_msb.as_mut_slice(),
                    &mut gray2_used,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    generated_icons::ICON_FOLDER_DARK_MASK,
                    generated_icons::ICON_FOLDER_LIGHT_MASK,
                ),
                1 => Self::draw_icon_gray2(
                    self.display_buffers,
                    self.gray2_lsb.as_mut_slice(),
                    self.gray2_msb.as_mut_slice(),
                    &mut gray2_used,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    generated_icons::ICON_GEAR_DARK_MASK,
                    generated_icons::ICON_GEAR_LIGHT_MASK,
                ),
                _ => Self::draw_icon_gray2(
                    self.display_buffers,
                    self.gray2_lsb.as_mut_slice(),
                    self.gray2_msb.as_mut_slice(),
                    &mut gray2_used,
                    icon_x,
                    icon_y,
                    icon_size,
                    icon_size,
                    generated_icons::ICON_BATTERY_DARK_MASK,
                    generated_icons::ICON_BATTERY_LIGHT_MASK,
                ),
            }
            let text_color = if is_selected {
                BinaryColor::On
            } else {
                BinaryColor::Off
            };
            let label_style = MonoTextStyle::new(&FONT_10X20, text_color);
            if *label != "" {
                let label_width = (label.len() as i32) * 10;
                let label_x = x + (action_width - label_width) / 2;
                Text::new(
                    label,
                    Point::new(label_x, y + action_height - 12),
                    label_style,
                )
                .draw(self.display_buffers)
                .ok();
            } else {
                let text = match self.battery_percent {
                    Some(value) => format!("{}%", value),
                    None => "--%".to_string(),
                };
                let label_width = (text.len() as i32) * 10;
                let label_x = x + (action_width - label_width) / 2;
                Text::new(
                    &text,
                    Point::new(label_x, y + action_height - 12),
                    label_style,
                )
                .draw(self.display_buffers)
                .ok();
            }
        }

        (gray2_used, draw_count)
    }

    fn draw_icon_gray2(
        buffers: &mut DisplayBuffers,
        gray2_lsb: &mut [u8],
        gray2_msb: &mut [u8],
        gray2_used: &mut bool,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        dark_mask: &[u8],
        light_mask: &[u8],
    ) {
        if width <= 0 || height <= 0 {
            return;
        }
        let width_u = width as usize;
        let height_u = height as usize;
        let expected = (width_u * height_u + 7) / 8;
        if dark_mask.len() != expected || light_mask.len() != expected {
            return;
        }
        for yy in 0..height_u {
            for xx in 0..width_u {
                let idx = yy * width_u + xx;
                let byte = idx / 8;
                let bit = 7 - (idx % 8);
                let dark = (dark_mask[byte] >> bit) & 1 == 1;
                let light = (light_mask[byte] >> bit) & 1 == 1;
                if !dark && !light {
                    continue;
                }
                *gray2_used = true;
                let dst_x = x + xx as i32;
                let dst_y = y + yy as i32;
                if dark {
                    buffers.set_pixel(dst_x, dst_y, BinaryColor::Off);
                } else {
                    buffers.set_pixel(dst_x, dst_y, BinaryColor::On);
                }
                let Some((fx, fy)) = Self::map_display_point(buffers.rotation(), dst_x, dst_y) else {
                    continue;
                };
                let dst_idx = fy * FB_WIDTH + fx;
                let dst_byte = dst_idx / 8;
                let dst_bit = 7 - (dst_idx % 8);
                if light {
                    gray2_lsb[dst_byte] |= 1 << dst_bit;
                }
                if dark {
                    gray2_msb[dst_byte] |= 1 << dst_bit;
                }
            }
        }
    }

    fn merge_bw_into_gray2(&mut self) {
        let size = self.display_buffers.size();
        let width = size.width as i32;
        let height = size.height as i32;
        for y in 0..height {
            for x in 0..width {
                if self.read_pixel(x, y) {
                    continue;
                }
                let Some((fx, fy)) =
                    Self::map_display_point(self.display_buffers.rotation(), x, y)
                else {
                    continue;
                };
                let idx = fy * FB_WIDTH + fx;
                let byte = idx / 8;
                let bit = 7 - (idx % 8);
                self.gray2_lsb[byte] |= 1 << bit;
                self.gray2_msb[byte] |= 1 << bit;
            }
        }
    }

    fn draw_menu(&mut self, display: &mut impl crate::display::Display) {
        let mut labels: Vec<String> = Vec::with_capacity(self.home.entries.len());
        for entry in &self.home.entries {
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

        let title = self.home.menu_title();
        let mut list = ListView::new(&items);
        list.title = Some(title.as_str());
        list.footer = Some("Up/Down: select  Confirm: open  Back: up");
        list.empty_label = Some("No entries found in /images");
        list.selected = self.home.selected;
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

    fn draw_settings(&mut self, display: &mut impl crate::display::Display) {
        self.display_buffers.clear(BinaryColor::On).ok();

        let heading_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        let body_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);

        let heading = "TernReader Firmware";
        let heading_pos = Point::new(LIST_MARGIN_X, HEADER_Y + 10);
        Text::new(heading, heading_pos, heading_style)
            .draw(self.display_buffers)
            .ok();
        Text::new(heading, Point::new(heading_pos.x + 1, heading_pos.y), heading_style)
            .draw(self.display_buffers)
            .ok();

        let logo_w = generated_icons::LOGO_WIDTH as i32;
        let logo_h = generated_icons::LOGO_HEIGHT as i32;
        let size = self.display_buffers.size();
        let logo_x = ((size.width as i32) - logo_w) / 2;
        let logo_y = heading_pos.y + 24;
        let mut gray2_used = false;
        Self::draw_icon_gray2(
            self.display_buffers,
            self.gray2_lsb.as_mut_slice(),
            self.gray2_msb.as_mut_slice(),
            &mut gray2_used,
            logo_x,
            logo_y,
            logo_w,
            logo_h,
            generated_icons::LOGO_DARK_MASK,
            generated_icons::LOGO_LIGHT_MASK,
        );

        let version_line = format!("Version: {}", build_info::VERSION);
        let time_line = format!("Build time: {}", build_info::BUILD_TIME);

        let details_y = logo_y + logo_h + 12;
        Text::new(&version_line, Point::new(LIST_MARGIN_X, details_y), body_style)
            .draw(self.display_buffers)
            .ok();
        Text::new(&time_line, Point::new(LIST_MARGIN_X, details_y + 24), body_style)
            .draw(self.display_buffers)
            .ok();

        Text::new(
            "Press Back to return",
            Point::new(LIST_MARGIN_X, details_y + 52),
            body_style,
        )
        .draw(self.display_buffers)
        .ok();

        if gray2_used {
            self.merge_bw_into_gray2();
            let lsb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                self.gray2_lsb.as_slice().try_into().unwrap();
            let msb_buf: &[u8; crate::framebuffer::BUFFER_SIZE] =
                self.gray2_msb.as_slice().try_into().unwrap();
            display.copy_grayscale_buffers(lsb_buf, msb_buf);
            display.display_absolute_grayscale(GrayscaleMode::Fast);
        } else {
            let mut rq = RenderQueue::default();
            rq.push(
                Rect::new(0, 0, size.width as i32, size.height as i32),
                RefreshMode::Full,
            );
            flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Full);
        }
    }


    fn draw_image_viewer(&mut self, display: &mut impl crate::display::Display) {
        let mut ctx = ImageViewerContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            source: self.source,
            wake_restore_only: &mut self.wake_restore_only,
        };
        if let Err(err) = self.image_viewer.draw(&mut ctx, display) {
            self.set_error(err);
        }
    }



    fn draw_book_reader(&mut self, display: &mut impl crate::display::Display) {
        let mut ctx = BookReaderContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            source: self.source,
            full_refresh: &mut self.full_refresh,
        };
        if let Err(err) = self.book_reader.draw_book(&mut ctx, display) {
            self.set_error(err);
        }
    }

    fn draw_toc_view(&mut self, display: &mut impl crate::display::Display) {
        let mut ctx = BookReaderContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            source: self.source,
            full_refresh: &mut self.full_refresh,
        };
        if let Err(err) = self.book_reader.draw_toc(&mut ctx, display) {
            self.set_error(err);
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
        if self.sleep_wallpaper_gray2 {
            let lsb: &[u8; BUFFER_SIZE] = self.gray2_lsb.as_slice().try_into().unwrap();
            let msb: &[u8; BUFFER_SIZE] = self.gray2_msb.as_slice().try_into().unwrap();
            display.copy_grayscale_buffers(lsb, msb);
            display.display_absolute_grayscale(GrayscaleMode::Fast);
        }
    }

    fn draw_sleep_wallpaper(&mut self) {
        self.sleep_wallpaper_gray2 = false;
        self.sleep_wallpaper_trbk_open = false;
        log::info!(
            "Sleep wallpaper: state={:?} sleep_from_home={} current_image={} current_book={} last_viewed={:?}",
            self.state,
            self.sleep_from_home,
            self.image_viewer.has_image(),
            self.book_reader.current_book.is_some(),
            self.last_viewed_entry
        );
        if self.image_viewer.has_image() {
            if let Some(image) = self.image_viewer.take_image() {
                self.render_wallpaper(&image);
                self.image_viewer.restore_image(image);
            }
            return;
        }
        if self.book_reader.current_book.is_some() {
            if let Ok(image) = self.source.trbk_image(0) {
                self.render_wallpaper(&image);
            }
            return;
        }
        if self.state == AppState::StartMenu || self.sleep_from_home {
            let recents = self.collect_recent_paths();
            log::info!("Sleep wallpaper recents: {:?}", recents);
            let recents = self.collect_recent_paths();
            if let Some(path) = recents.first() {
                log::info!("Sleep wallpaper path: {}", path);
                if let Some(image) = self.load_sleep_wallpaper_from_path(path) {
                    log::info!("Sleep wallpaper loaded for {}", path);
                    self.render_wallpaper(&image);
                    if self.sleep_wallpaper_trbk_open {
                        self.source.close_trbk();
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
        self.render_sleep_fallback_logo();
        log::info!("Sleep wallpaper: none rendered");
    }

    fn render_sleep_fallback_logo(&mut self) {
        self.gray2_lsb.fill(0);
        self.gray2_msb.fill(0);
        let size = self.display_buffers.size();
        let logo_w = generated_icons::LOGO_WIDTH as i32;
        let logo_h = generated_icons::LOGO_HEIGHT as i32;
        let x = ((size.width as i32) - logo_w) / 2;
        let y = ((size.height as i32) - logo_h) / 2;
        let mut gray2_used = false;
        Self::draw_icon_gray2(
            self.display_buffers,
            self.gray2_lsb.as_mut_slice(),
            self.gray2_msb.as_mut_slice(),
            &mut gray2_used,
            x,
            y,
            logo_w,
            logo_h,
            generated_icons::LOGO_DARK_MASK,
            generated_icons::LOGO_LIGHT_MASK,
        );
        if gray2_used {
            self.sleep_wallpaper_gray2 = true;
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
            if matches!(image, Some(ImageData::Gray2Stream { .. })) {
                self.sleep_wallpaper_trbk_open = true;
            } else {
                self.source.close_trbk();
            }
            return image;
        }
        if lower.ends_with(".tri") || lower.ends_with(".trimg") {
            return self.source.load(&parts, &entry).ok();
        }
        None
    }

    fn render_wallpaper(&mut self, image: &ImageData) {
        match image {
            ImageData::Gray2 { width, height, data } => {
                self.gray2_lsb.fill(0);
                self.gray2_msb.fill(0);
                let plane = (((*width as usize) * (*height as usize)) + 7) / 8;
                if data.len() >= plane * 3 {
                    Self::render_gray2_contain(
                        self.display_buffers,
                        self.display_buffers.rotation(),
                        &mut self.gray2_lsb,
                        &mut self.gray2_msb,
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
                self.gray2_lsb.fill(0);
                self.gray2_msb.fill(0);
                let target = self.display_buffers.size();
                let target_w = target.width as i32;
                let target_h = target.height as i32;
                let offset_x = ((target_w - *width as i32) / 2).max(0);
                let offset_y = ((target_h - *height as i32) / 2).max(0);
                if self
                    .source
                    .load_gray2_stream_region(
                        key,
                        *width,
                        *height,
                        self.display_buffers.rotation(),
                        self.display_buffers.get_active_buffer_mut(),
                        &mut self.gray2_lsb,
                        &mut self.gray2_msb,
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
        self.home.path = parts;
        self.refresh_entries();
        let idx = self.home.entries.iter().position(|entry| entry.name == file);
        if let Some(index) = idx {
            self.open_index(index);
            if let Some(book) = &self.book_reader.current_book {
                if let Some(name) = &self.current_entry {
                    if let Some(page) = self.book_positions.get(name).copied() {
                        if page < book.page_count {
                            self.book_reader.current_page = page;
                            self.book_reader.current_page_ops = self.source.trbk_page(self.book_reader.current_page).ok();
                            self.full_refresh = true;
                            self.book_reader.book_turns_since_full = 0;
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
        self.home.path = parts;
        self.refresh_entries();
        let idx = self.home.entries.iter().position(|entry| entry.name == file);
        if let Some(index) = idx {
            self.home.selected = index;
            self.open_index(index);
        } else {
            self.set_error(ImageError::Message("Recent entry not found.".into()));
        }
    }

    fn ensure_start_menu_cache(&mut self, recents: &[String]) {
        let same = recents.len() == self.home.start_menu_cache.len()
            && recents
                .iter()
                .zip(self.home.start_menu_cache.iter())
                .all(|(path, cached)| path == &cached.path);
        if same {
            return;
        }
        self.home.start_menu_cache.clear();
        for path in recents {
            let (title, image) = self.load_recent_preview(path);
            self.home.start_menu_cache.push(RecentPreview {
                path: path.clone(),
                title,
                image,
            });
        }
        self.home.start_menu_need_base_refresh = true;
    }

    fn load_recent_preview(&mut self, path: &str) -> (String, Option<ImageData>) {
        let label_fallback = basename_from_path(path);
        if let Some(image) = self.source.load_thumbnail(path) {
            let title = self
                .source
                .load_thumbnail_title(path)
                .filter(|value| !value.is_empty())
                .unwrap_or(label_fallback);
            if let Some(mono) = self.thumbnail_to_mono(&image) {
                if !matches!(image, ImageData::Mono1 { .. }) {
                    self.source.save_thumbnail(path, &mono);
                }
                return (title, Some(mono));
            }
            let needs_resize = match &image {
                ImageData::Mono1 { width, height, .. }
                | ImageData::Gray8 { width, height, .. }
                | ImageData::Gray2 { width, height, .. }
                | ImageData::Gray2Stream { width, height, .. } => {
                    *width != START_MENU_RECENT_THUMB as u32
                        || *height != START_MENU_RECENT_THUMB as u32
                }
            };
            if needs_resize {
                if let Some(thumb) =
                    self.thumbnail_from_image(&image, START_MENU_RECENT_THUMB as u32)
                {
                    self.source.save_thumbnail(path, &thumb);
                    return (title, Some(thumb));
                }
            }
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
            if let ImageData::Gray2Stream { width, height, key } = &image {
                if let Some(thumb) = self.source.load_gray2_stream_thumbnail(
                    key,
                    *width,
                    *height,
                    74,
                    74,
                ) {
                    self.source.save_thumbnail(path, &thumb);
                    return (label_fallback, Some(thumb));
                }
            }
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
                if let ImageData::Gray2Stream { width, height, key } = &image {
                    if let Some(thumb) = self.source.load_gray2_stream_thumbnail(
                        key,
                        *width,
                        *height,
                        START_MENU_RECENT_THUMB as u32,
                        START_MENU_RECENT_THUMB as u32,
                    ) {
                        return Some(thumb);
                    }
                }
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
        let mut bits = vec![0xFF; dst_len];
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
                let lum = Self::adjust_thumbnail_luma(lum);
                if lum >= 128 {
                    bits[dst_byte] |= 1 << dst_bit;
                } else {
                    bits[dst_byte] &= !(1 << dst_bit);
                }
            }
        }
        Some(ImageData::Mono1 {
            width: dst_w,
            height: dst_h,
            bits,
        })
    }

    fn thumbnail_to_mono(&self, image: &ImageData) -> Option<ImageData> {
        match image {
            ImageData::Mono1 { .. } => Some(image.clone()),
            ImageData::Gray8 { width, height, pixels } => {
                let plane = ((*width as usize * *height as usize) + 7) / 8;
                let mut bits = vec![0xFF; plane];
                for idx in 0..(*width as usize * *height as usize) {
                    let byte = idx / 8;
                    let bit = 7 - (idx % 8);
                    let lum = pixels.get(idx).copied().unwrap_or(255);
                    let lum = Self::adjust_thumbnail_luma(lum);
                    if lum >= 128 {
                        bits[byte] |= 1 << bit;
                    } else {
                        bits[byte] &= !(1 << bit);
                    }
                }
                Some(ImageData::Mono1 {
                    width: *width,
                    height: *height,
                    bits,
                })
            }
            ImageData::Gray2 { width, height, data } => {
                let plane = ((*width as usize * *height as usize) + 7) / 8;
                if data.len() < plane * 3 {
                    return None;
                }
                let mut bits = vec![0xFF; plane];
                for idx in 0..(*width as usize * *height as usize) {
                    let byte = idx / 8;
                    let bit = 7 - (idx % 8);
                    let bw = (data[byte] >> bit) & 1;
                    let l = (data[plane + byte] >> bit) & 1;
                    let m = (data[plane * 2 + byte] >> bit) & 1;
                    let lum = match (m, l, bw) {
                        (0, 0, 1) => 255,
                        (0, 1, 1) => 192,
                        (1, 0, 0) => 128,
                        (1, 1, 0) => 64,
                        _ => 0,
                    };
                    let lum = Self::adjust_thumbnail_luma(lum);
                    if lum >= 128 {
                        bits[byte] |= 1 << bit;
                    } else {
                        bits[byte] &= !(1 << bit);
                    }
                }
                Some(ImageData::Mono1 {
                    width: *width,
                    height: *height,
                    bits,
                })
            }
            ImageData::Gray2Stream { .. } => None,
        }
    }

    fn adjust_thumbnail_luma(lum: u8) -> u8 {
        let mut value = ((lum as i32 - 128) * 13) / 10 + 128;
        if value < 0 {
            value = 0;
        } else if value > 255 {
            value = 255;
        }
        value as u8
    }



    fn current_resume_string(&self) -> Option<String> {
        if self.state == AppState::StartMenu {
            return Some("HOME".to_string());
        }
        let name = self
            .current_entry
            .clone()
            .or_else(|| self.last_viewed_entry.clone())
            .or_else(|| self.home.current_entry_name_owned())?;
        Some(name)
    }

    fn save_resume_checked(&mut self) -> bool {
        let resume_debug = format!(
            "state={:?} current_entry={:?} last_viewed_entry={:?} path={:?} selected={} has_book={} current_page={} last_rendered={:?}",
            self.state,
            self.current_entry,
            self.last_viewed_entry,
            self.home.path,
            self.home.selected,
            self.book_reader.current_book.is_some(),
            self.book_reader.current_page,
            self.book_reader.last_rendered_page
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
        true
    }

    fn update_book_position(&mut self) {
        if self.book_reader.current_book.is_some() {
            if let Some(name) = self
                .current_entry
                .clone()
                .or_else(|| self.last_viewed_entry.clone())
            {
                let prev = self.book_positions.insert(name, self.book_reader.current_page);
                if prev != Some(self.book_reader.current_page) {
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

}
