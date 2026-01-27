extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::{
    Drawable,
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point, Primitive, Size},
    primitives::{PrimitiveStyle, Rectangle},
    text::Text,
};

use crate::{
    display::RefreshMode,
    framebuffer::DisplayBuffers,
    image_viewer::{ImageData, ImageEntry, ImageError, ImageSource},
    input,
};

const LIST_TOP: i32 = 60;
const LINE_HEIGHT: i32 = 24;
const LIST_MARGIN_X: i32 = 16;
const HEADER_Y: i32 = 24;

pub struct Application<'a, S: ImageSource> {
    dirty: bool,
    display_buffers: &'a mut DisplayBuffers,
    source: &'a mut S,
    images: Vec<ImageEntry>,
    selected: usize,
    state: AppState,
    current_image: Option<ImageData>,
    error_message: Option<String>,
    sleep_transition: bool,
    wake_transition: bool,
    full_refresh: bool,
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
        let mut app = Application {
            dirty: true,
            display_buffers,
            source,
            images: Vec::new(),
            selected: 0,
            state: AppState::Menu,
            current_image: None,
            error_message: None,
            sleep_transition: false,
            wake_transition: false,
            full_refresh: true,
        };
        app.refresh_images();
        app
    }

    pub fn update(&mut self, buttons: &input::ButtonState) {
        if buttons.is_pressed(input::Buttons::Power) {
            if self.state == AppState::Sleeping {
                self.source.wake();
                self.state = AppState::Menu;
                self.wake_transition = true;
                self.full_refresh = true;
                self.dirty = true;
                self.refresh_images();
                return;
            }
        }

        match self.state {
            AppState::Menu => {
                if buttons.is_pressed(input::Buttons::Up) {
                    if !self.images.is_empty() {
                        self.selected = self.selected.saturating_sub(1);
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Down) {
                    if !self.images.is_empty() {
                        self.selected = (self.selected + 1).min(self.images.len() - 1);
                    }
                    self.dirty = true;
                } else if buttons.is_pressed(input::Buttons::Confirm) {
                    self.open_selected();
                } else if buttons.is_pressed(input::Buttons::Back) {
                    self.refresh_images();
                }
            }
            AppState::Viewing => {
                // No input handling; we immediately sleep after drawing.
            }
            AppState::Sleeping => {
                if buttons.is_pressed(input::Buttons::Power) {
                    self.source.wake();
                    self.state = AppState::Menu;
                    self.wake_transition = true;
                    self.full_refresh = true;
                    self.dirty = true;
                    self.refresh_images();
                }
            }
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
                // Keep the image on screen while sleeping.
            }
            AppState::Error => self.draw_error(display),
        }
        self.full_refresh = false;
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
        if self.images.is_empty() {
            self.error_message = Some("No images found in /images.".into());
            self.state = AppState::Error;
            self.dirty = true;
            return;
        }
        if let Some(entry) = self.images.get(self.selected).cloned() {
            match self.source.load(&entry) {
                Ok(image) => {
                    self.current_image = Some(image);
                    self.state = AppState::Viewing;
                    self.full_refresh = true;
                    self.dirty = true;
                }
                Err(err) => self.set_error(err),
            }
        }
    }

    fn refresh_images(&mut self) {
        match self.source.refresh() {
            Ok(images) => {
                self.images = images;
                self.current_image = None;
                if self.selected >= self.images.len() {
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
        self.display_buffers.clear(BinaryColor::On).ok();

        let header_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new("Image Viewer", Point::new(LIST_MARGIN_X, HEADER_Y), header_style)
            .draw(self.display_buffers)
            .ok();

        let footer_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new(
            "Up/Down: select  Confirm: view  Back: refresh",
            Point::new(LIST_MARGIN_X, (self.display_buffers.size().height as i32) - 16),
            footer_style,
        )
        .draw(self.display_buffers)
        .ok();

        if self.images.is_empty() {
            Text::new(
                "No images found in /images",
                Point::new(LIST_MARGIN_X, LIST_TOP),
                header_style,
            )
            .draw(self.display_buffers)
            .ok();
        } else {
            let max_lines = ((self.display_buffers.size().height as i32 - LIST_TOP - 40)
                / LINE_HEIGHT)
                .max(1) as usize;
            let start = self.selected.saturating_sub(max_lines / 2);
            let end = (start + max_lines).min(self.images.len());

            for (idx, entry) in self.images[start..end].iter().enumerate() {
                let actual_idx = start + idx;
                let y = LIST_TOP + (idx as i32 * LINE_HEIGHT);
                if actual_idx == self.selected {
                    Rectangle::new(
                        Point::new(0, y - 18),
                        Size::new(self.display_buffers.size().width, LINE_HEIGHT as u32),
                    )
                    .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                    .draw(self.display_buffers)
                    .ok();
                    let selected_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::On);
                    Text::new(&entry.name, Point::new(LIST_MARGIN_X, y), selected_style)
                        .draw(self.display_buffers)
                        .ok();
                } else {
                    Text::new(&entry.name, Point::new(LIST_MARGIN_X, y), header_style)
                        .draw(self.display_buffers)
                        .ok();
                }
            }
        }

        display.display(
            self.display_buffers,
            if self.full_refresh {
                RefreshMode::Full
            } else {
                RefreshMode::Fast
            },
        );
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
        display.display(self.display_buffers, RefreshMode::Full);
    }

    fn draw_image(&mut self, display: &mut impl crate::display::Display) {
        let Some(image) = self.current_image.take() else {
            self.set_error(ImageError::Decode);
            return;
        };
        self.render_image(&image);
        self.current_image = Some(image);
        display.display(self.display_buffers, RefreshMode::Full);
        self.source.sleep();
        self.state = AppState::Sleeping;
        self.sleep_transition = true;
    }

    fn render_image(&mut self, image: &ImageData) {
        self.display_buffers.clear(BinaryColor::On).ok();

        let target = self.display_buffers.size();
        let target_w = target.width.max(1);
        let target_h = target.height.max(1);
        let img_w = image.width.max(1);
        let img_h = image.height.max(1);

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
                if idx >= image.pixels.len() {
                    continue;
                }
                let lum = image.pixels[idx];
                let threshold = (bayer[(y as usize) & 3][(x as usize) & 3] * 16 + 8) as u8;
                let color = if lum < threshold {
                    BinaryColor::Off
                } else {
                    BinaryColor::On
                };
                self.display_buffers
                    .set_pixel(offset_x + x as i32, offset_y + y as i32, color);
            }
        }
    }
}
