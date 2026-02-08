extern crate alloc;

use alloc::{format, string::String};
use alloc::vec::Vec;
use alloc::vec;

use embedded_graphics::{
    Drawable,
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point, Primitive},
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
        home::{
            HomeAction,
            HomeIcons,
            HomeOpen,
            HomeOpenError,
            HomeRenderContext,
            HomeState,
            MenuAction,
        },
        image_viewer::{ImageViewerContext, ImageViewerState},
        settings::{draw_settings, SettingsContext},
        system::{ApplyResumeOutcome, ResumeContext, SleepWallpaperIcons, SystemRenderContext, SystemState},
    },
    build_info,
    display::RefreshMode,
    framebuffer::{DisplayBuffers, Rotation},
    image_viewer::{AppSource, ImageEntry, ImageError},
    input,
    ui::{flush_queue, Rect, RenderQueue},
};

const LIST_MARGIN_X: i32 = 16;
const HEADER_Y: i32 = 24;
const PAGE_INDICATOR_MARGIN: i32 = 12;
const PAGE_INDICATOR_Y: i32 = 24;
pub struct Application<'a, S: AppSource> {
    dirty: bool,
    display_buffers: &'a mut DisplayBuffers,
    source: &'a mut S,
    home: HomeState,
    state: AppState,
    image_viewer: ImageViewerState,
    book_reader: BookReaderState,
    system: SystemState,
    current_entry: Option<String>,
    last_viewed_entry: Option<String>,
    error_message: Option<String>,
    gray2_lsb: Vec<u8>,
    gray2_msb: Vec<u8>,
    exit_from: ExitFrom,
    exit_overlay_drawn: bool,
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
            .collect();
        let recent_entries = source.load_recent_entries();
        let system = SystemState::new(resume_name, book_positions, recent_entries);
        let mut app = Application {
            dirty: true,
            display_buffers,
            source,
            home: HomeState::new(),
            state: AppState::StartMenu,
            image_viewer: ImageViewerState::new(),
            book_reader: BookReaderState::new(),
            system,
            current_entry: None,
            last_viewed_entry: None,
            error_message: None,
            gray2_lsb: vec![0u8; crate::framebuffer::BUFFER_SIZE],
            gray2_msb: vec![0u8; crate::framebuffer::BUFFER_SIZE],
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
            if let Some(overlay) = self.system.sleep_overlay.take() {
                SystemState::restore_rect_bits(self.display_buffers, &overlay);
                if self.book_reader.current_book.is_some() {
                    self.set_state_book_viewing();
                    self.system.full_refresh = true;
                    self.system.wake_restore_only = false;
                } else if self.image_viewer.has_image() {
                    self.set_state_viewing();
                    self.system.wake_restore_only = true;
                } else {
                    self.set_state_start_menu(true);
                }
                resumed_viewer = true;
            } else {
                self.set_state_start_menu(true);
            }
            self.system.on_wake();
            self.dirty = true;
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
            self.system.reset_idle();
        }

        match self.state {
            AppState::StartMenu => {
                let recents = self.system.collect_recent_paths(self.last_viewed_entry.as_ref());
                match self.home.handle_start_menu_input(&recents, buttons) {
                    HomeAction::OpenRecent(path) => {
                        match self.home.open_recent_path(self.source, &path) {
                            Ok(()) => {
                                let index = self.home.selected;
                                self.open_index(index);
                            }
                            Err(err) => self.set_error(err),
                        }
                    }
                    HomeAction::OpenFileBrowser => {
                        self.state = AppState::Menu;
                        self.home.selected = 0;
                        self.refresh_entries();
                        self.dirty = true;
                    }
                    HomeAction::OpenSettings => {
                        self.set_state_settings();
                    }
                    HomeAction::None => {
                        if Self::has_input(buttons) {
                            self.dirty = true;
                        } else {
                            if self.system.add_idle(elapsed_ms) {
                                self.start_sleep_request();
                            }
                        }
                    }
                }
                if !Self::has_input(buttons) {
                    if self.system.add_idle(elapsed_ms) {
                        self.start_sleep_request();
                    }
                }
            }
            AppState::Menu => {
                match self.home.handle_menu_input(buttons) {
                    MenuAction::OpenSelected => {
                        self.open_selected();
                    }
                    MenuAction::Back => {
                        if !self.home.path.is_empty() {
                            self.home.path.pop();
                            self.refresh_entries();
                        } else {
                            self.set_state_start_menu(true);
                        }
                    }
                    MenuAction::Dirty => {
                        self.dirty = true;
                    }
                    MenuAction::None => {
                        if self.system.add_idle(elapsed_ms) {
                            self.start_sleep_request();
                        }
                    }
                }
            }
            AppState::Settings => {
                if buttons.is_pressed(input::Buttons::Back)
                    || buttons.is_pressed(input::Buttons::Confirm)
                {
                    self.set_state_start_menu(true);
                } else {
                    if self.system.add_idle(elapsed_ms) {
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
                    if self.system.add_idle(elapsed_ms) {
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
                    self.set_state_toc();
                } else if result.dirty {
                    self.dirty = true;
                } else {
                    if self.system.add_idle(elapsed_ms) {
                        self.start_sleep_request();
                    }
                }
            }
            AppState::Toc => {
                let result = self.book_reader.handle_toc_input(buttons);
                if result.exit {
                    self.set_state_book_viewing();
                } else if result.jumped {
                    self.set_state_book_viewing();
                } else if result.dirty {
                    self.dirty = true;
                } else {
                    if self.system.add_idle(elapsed_ms) {
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
                    self.error_message = None;
                    self.set_state_start_menu(true);
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
                    ExitFrom::Image => self.exit_image(),
                    ExitFrom::Book => self.exit_book(),
                }
                self.state = AppState::StartMenu;
                self.home.start_menu_cache.clear();
                self.set_state_start_menu(true);
            }
            AppState::Toc => self.draw_toc_view(display),
            AppState::SleepingPending => {
                self.draw_sleeping_indicator(display);
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
                let outcome = self.system.save_resume_or_error(ResumeContext {
                    source: self.source,
                    resume_debug: &resume_debug,
                    in_start_menu: self.state == AppState::StartMenu,
                    current_entry: self.current_entry.as_ref(),
                    last_viewed_entry: self.last_viewed_entry.as_ref(),
                    home_current_entry: self.home.current_entry_name_owned(),
                    book_reader: &self.book_reader,
                });
                if outcome.is_ok() {
                    self.state = AppState::Sleeping;
                    self.system.start_sleep_overlay();
                    self.draw_sleep_overlay(display);
                } else if let Err(message) = outcome {
                    self.set_state_error_message(message);
                }
            }
            AppState::Sleeping => {
                self.draw_sleep_overlay(display);
            }
            AppState::Error => self.draw_error(display),
        }
        self.system.full_refresh = false;
        if self.state == AppState::Error && self.system.sleep_after_error {
            self.system.sleep_after_error = false;
            self.state = AppState::Sleeping;
            self.system.start_sleep_overlay();
            self.dirty = true;
        }
    }

    pub fn with_source<R>(&mut self, f: impl FnOnce(&mut S) -> R) -> R {
        f(self.source)
    }

    pub fn source_mut(&mut self) -> &mut S {
        self.source
    }

    fn has_input(buttons: &input::ButtonState) -> bool {
        use input::Buttons::*;
        let list = [Back, Confirm, Left, Right, Up, Down, Power];
        list.iter()
            .any(|b| buttons.is_pressed(*b) || buttons.is_held(*b))
    }

    pub fn take_sleep_transition(&mut self) -> bool {
        self.system.take_sleep_transition()
    }

    pub fn take_wake_transition(&mut self) -> bool {
        self.system.take_wake_transition()
    }

    pub fn set_battery_percent(&mut self, percent: Option<u8>) {
        if self.system.set_battery_percent(percent) && self.state == AppState::StartMenu {
            self.dirty = true;
        }
    }

    fn open_selected(&mut self) {
        let action = match self.home.open_selected() {
            Ok(action) => action,
            Err(HomeOpenError::Empty) => {
                self.error_message = Some("No entries found in /images.".into());
                self.state = AppState::Error;
                self.dirty = true;
                return;
            }
        };
        match action {
            HomeOpen::EnterDir => {
                self.refresh_entries();
                if matches!(self.state, AppState::Error) {
                    self.home.path.pop();
                    self.refresh_entries();
                    self.set_error(ImageError::Message("Folder open failed.".into()));
                }
            }
            HomeOpen::OpenFile(entry) => {
                self.open_file_entry(entry);
            }
        }
    }

    fn open_index(&mut self, index: usize) {
        let Some(action) = self.home.open_index(index) else {
            return;
        };
        match action {
            HomeOpen::EnterDir => {}
            HomeOpen::OpenFile(entry) => self.open_file_entry(entry),
        }
    }

    fn open_file_entry(&mut self, entry: ImageEntry) {
        if is_trbk(&entry.name) {
            self.open_book_entry(entry);
            return;
        }
        if is_epub(&entry.name) {
            self.set_error(ImageError::Message(
                "EPUB files must be converted to .trbk.".into(),
            ));
            return;
        }
        self.open_image_entry(entry);
    }

    fn open_book_entry(&mut self, entry: ImageEntry) {
        let entry_name = self.home.entry_path_string(&entry);
        match self.book_reader.open(
            self.source,
            &self.home.path,
            &entry,
            &entry_name,
            &self.system.book_positions,
        ) {
            Ok(()) => {
                self.current_entry = Some(entry_name.clone());
                self.last_viewed_entry = Some(entry_name.clone());
                self.system.mark_recent(entry_name);
                log::info!("Opened book entry: {:?}", self.current_entry);
                self.set_state_book_viewing();
            }
            Err(err) => self.set_error(err),
        }
    }

    fn open_image_entry(&mut self, entry: ImageEntry) {
        match self.image_viewer.open(self.source, &self.home.path, &entry) {
            Ok(()) => {
                let entry_name = self.home.entry_path_string(&entry);
                self.current_entry = Some(entry_name.clone());
                self.last_viewed_entry = Some(entry_name.clone());
                self.system.mark_recent(entry_name);
                log::info!("Opened image entry: {:?}", self.current_entry);
                self.set_state_viewing();
                self.system.reset_idle();
                self.system.sleep_overlay = None;
                self.system.clear_sleep_overlay_pending();
            }
            Err(err) => self.set_error(err),
        }
    }

    fn exit_image(&mut self) {
        self.source.save_resume(None);
        self.system.save_recent_entries_now(self.source);
    }

    fn exit_book(&mut self) {
        self.system.update_book_position(
            &self.book_reader,
            self.current_entry.as_ref(),
            self.last_viewed_entry.as_ref(),
        );
        self.system.save_book_positions_now(self.source);
        self.system.save_recent_entries_now(self.source);
        self.book_reader.close(self.source);
    }

    fn refresh_entries(&mut self) {
        match self.home.refresh_entries(self.source) {
            Ok(()) => {
                self.image_viewer.clear();
                self.book_reader.clear();
                if self.state != AppState::StartMenu {
                    self.set_state_menu();
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
        self.set_state_error_message(message);
    }

    fn set_state_start_menu(&mut self, need_base_refresh: bool) {
        self.state = AppState::StartMenu;
        self.home.start_menu_need_base_refresh = need_base_refresh;
        self.dirty = true;
    }

    fn set_state_settings(&mut self) {
        self.state = AppState::Settings;
        self.dirty = true;
    }

    fn set_state_menu(&mut self) {
        self.state = AppState::Menu;
        self.dirty = true;
    }

    fn set_state_viewing(&mut self) {
        self.state = AppState::Viewing;
        self.system.full_refresh = true;
        self.dirty = true;
    }

    fn set_state_book_viewing(&mut self) {
        self.state = AppState::BookViewing;
        self.system.full_refresh = true;
        self.dirty = true;
    }

    fn set_state_toc(&mut self) {
        self.state = AppState::Toc;
        self.dirty = true;
    }

    fn set_state_error_message(&mut self, message: String) {
        self.error_message = Some(message);
        self.state = AppState::Error;
        self.dirty = true;
    }


    fn draw_start_menu(&mut self, display: &mut impl crate::display::Display) {
        let recents = self.system.collect_recent_paths(self.last_viewed_entry.as_ref());
        let icons = HomeIcons {
            icon_size: generated_icons::ICON_SIZE as i32,
            folder_dark: generated_icons::ICON_FOLDER_DARK_MASK,
            folder_light: generated_icons::ICON_FOLDER_LIGHT_MASK,
            gear_dark: generated_icons::ICON_GEAR_DARK_MASK,
            gear_light: generated_icons::ICON_GEAR_LIGHT_MASK,
            battery_dark: generated_icons::ICON_BATTERY_DARK_MASK,
            battery_light: generated_icons::ICON_BATTERY_LIGHT_MASK,
        };
        let mut ctx = HomeRenderContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            source: self.source,
            full_refresh: self.system.full_refresh,
            battery_percent: self.system.battery_percent,
            icons,
            draw_trbk_image,
        };
        self.home.draw_start_menu(&mut ctx, display, &recents);
    }



    fn draw_menu(&mut self, display: &mut impl crate::display::Display) {
        let icons = HomeIcons {
            icon_size: generated_icons::ICON_SIZE as i32,
            folder_dark: generated_icons::ICON_FOLDER_DARK_MASK,
            folder_light: generated_icons::ICON_FOLDER_LIGHT_MASK,
            gear_dark: generated_icons::ICON_GEAR_DARK_MASK,
            gear_light: generated_icons::ICON_GEAR_LIGHT_MASK,
            battery_dark: generated_icons::ICON_BATTERY_DARK_MASK,
            battery_light: generated_icons::ICON_BATTERY_LIGHT_MASK,
        };
        let mut ctx = HomeRenderContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            source: self.source,
            full_refresh: self.system.full_refresh,
            battery_percent: self.system.battery_percent,
            icons,
            draw_trbk_image,
        };
        self.home.draw_menu(&mut ctx, display);
    }


    fn draw_error(&mut self, display: &mut impl crate::display::Display) {
        const ERROR_LIST_TOP: i32 = 60;
        self.display_buffers.clear(BinaryColor::On).ok();
        let header_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new("Error", Point::new(LIST_MARGIN_X, HEADER_Y), header_style)
            .draw(self.display_buffers)
            .ok();
        if let Some(message) = &self.error_message {
            Text::new(message, Point::new(LIST_MARGIN_X, ERROR_LIST_TOP), header_style)
                .draw(self.display_buffers)
                .ok();
        }
        Text::new(
            "Press Back to return",
            Point::new(LIST_MARGIN_X, ERROR_LIST_TOP + 40),
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
        let mut ctx = SettingsContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            logo_w: generated_icons::LOGO_WIDTH as i32,
            logo_h: generated_icons::LOGO_HEIGHT as i32,
            logo_dark: generated_icons::LOGO_DARK_MASK,
            logo_light: generated_icons::LOGO_LIGHT_MASK,
            version: build_info::VERSION,
            build_time: build_info::BUILD_TIME,
        };
        draw_settings(&mut ctx, display);
    }

    pub fn draw_usb_modal(
        &mut self,
        display: &mut impl crate::display::Display,
        title: &str,
        message: &str,
        footer: &str,
    ) {
        self.display_buffers.clear(BinaryColor::On).ok();
        let style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new(title, Point::new(16, 24), style)
            .draw(self.display_buffers)
            .ok();
        Text::new(message, Point::new(16, 60), style)
            .draw(self.display_buffers)
            .ok();
        Text::new(footer, Point::new(16, 100), style)
            .draw(self.display_buffers)
            .ok();
        display.display(self.display_buffers, RefreshMode::Full);
    }


    fn draw_image_viewer(&mut self, display: &mut impl crate::display::Display) {
        let mut ctx = ImageViewerContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            source: self.source,
            wake_restore_only: &mut self.system.wake_restore_only,
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
            full_refresh: &mut self.system.full_refresh,
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
            full_refresh: &mut self.system.full_refresh,
        };
        if let Err(err) = self.book_reader.draw_toc(&mut ctx, display) {
            self.set_error(err);
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

    fn draw_exiting_overlay(&mut self, display: &mut impl crate::display::Display) {
        let size = self.display_buffers.size();
        let text = "Exiting...";
        let text_w = (text.len() as i32) * 10;
        let padding_x = 10;
        let padding_y = 6;
        let rect_w = text_w + (padding_x * 2);
        let rect_h = 20 + (padding_y * 2);
        let x = (size.width as i32 - rect_w) / 2;
        let y = (size.height as i32 - rect_h) / 2;

        embedded_graphics::primitives::Rectangle::new(
            Point::new(x, y),
            embedded_graphics::geometry::Size::new(rect_w as u32, rect_h as u32),
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
        rq.push(Rect::new(x, y, rect_w, rect_h), RefreshMode::Fast);
        flush_queue(display, self.display_buffers, &mut rq, RefreshMode::Fast);
    }

    fn draw_sleep_overlay(&mut self, display: &mut impl crate::display::Display) {
        let logo = SleepWallpaperIcons {
            logo_w: generated_icons::LOGO_WIDTH as i32,
            logo_h: generated_icons::LOGO_HEIGHT as i32,
            logo_dark: generated_icons::LOGO_DARK_MASK,
            logo_light: generated_icons::LOGO_LIGHT_MASK,
        };
        let is_start_menu = self.state == AppState::StartMenu;
        let last_viewed_entry = &self.last_viewed_entry;
        let mut ctx = SystemRenderContext {
            display_buffers: self.display_buffers,
            gray2_lsb: self.gray2_lsb.as_mut_slice(),
            gray2_msb: self.gray2_msb.as_mut_slice(),
            source: self.source,
            image_viewer: &mut self.image_viewer,
            book_reader: &mut self.book_reader,
            last_viewed_entry,
            is_start_menu,
            logo,
        };
        self.system.process_sleep_overlay(&mut ctx, display);
    }

    fn try_resume(&mut self) {
        let outcome = self.system.try_resume();
        let outcome = self
            .system
            .apply_resume(outcome, &mut self.home, self.source);
        match outcome {
            ApplyResumeOutcome::None => {}
            ApplyResumeOutcome::Missing => {}
            ApplyResumeOutcome::Ready {
                entry,
                page,
                refreshed,
            } => {
                if refreshed {
                    self.image_viewer.clear();
                    self.book_reader.clear();
                    if self.state != AppState::StartMenu {
                        self.state = AppState::Menu;
                    }
                    self.error_message = None;
                    self.dirty = true;
                }
                self.open_file_entry(entry);
                if let Some(page) = page {
                    if let Some(book) = &self.book_reader.current_book {
                        if page < book.page_count {
                            self.book_reader.current_page = page;
                            self.book_reader.current_page_ops =
                                self.source.trbk_page(self.book_reader.current_page).ok();
                            self.system.full_refresh = true;
                            self.book_reader.book_turns_since_full = 0;
                            self.dirty = true;
                        }
                    }
                }
            }
        }
    }

    fn start_sleep_request(&mut self) {
        if self.state == AppState::Sleeping || self.state == AppState::SleepingPending {
            return;
        }
        self.system.start_sleep_request(self.state == AppState::StartMenu);
        self.state = AppState::SleepingPending;
        self.dirty = true;
    }

}
