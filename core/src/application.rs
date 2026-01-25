use embedded_graphics::{
    Drawable,
    mono_font::{MonoTextStyle, ascii::FONT_10X20},
    pixelcolor::BinaryColor,
    prelude::{DrawTarget, OriginDimensions, Point, Primitive, Size},
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::Text,
};

use crate::{
    display::RefreshMode,
    framebuffer::{DisplayBuffers, Rotation},
    input, test_image,
};

pub struct Application<'a> {
    dirty: bool,
    display_buffers: &'a mut DisplayBuffers,
    screen: usize,
    full_refresh: bool,
}

impl<'a> Application<'a> {
    pub fn new(display_buffers: &'a mut DisplayBuffers) -> Self {
        Application {
            dirty: true,
            display_buffers,
            screen: 0,
            full_refresh: true,
        }
    }

    pub fn update(&mut self, buttons: &input::ButtonState) {
        self.dirty |= buttons.is_pressed(input::Buttons::Confirm);
        if buttons.is_pressed(input::Buttons::Left) {
            self.display_buffers
                .set_rotation(match self.display_buffers.rotation() {
                    Rotation::Rotate0 => Rotation::Rotate270,
                    Rotation::Rotate90 => Rotation::Rotate0,
                    Rotation::Rotate180 => Rotation::Rotate90,
                    Rotation::Rotate270 => Rotation::Rotate180,
                });
            self.dirty = true;
        } else if buttons.is_pressed(input::Buttons::Right) {
            self.display_buffers
                .set_rotation(match self.display_buffers.rotation() {
                    Rotation::Rotate0 => Rotation::Rotate90,
                    Rotation::Rotate90 => Rotation::Rotate180,
                    Rotation::Rotate180 => Rotation::Rotate270,
                    Rotation::Rotate270 => Rotation::Rotate0,
                });
            self.dirty = true;
        } else if buttons.is_pressed(input::Buttons::Up) {
            self.screen = self.screen.wrapping_sub(1) % 3;
            self.dirty = true;
        } else if buttons.is_pressed(input::Buttons::Down) {
            self.screen = (self.screen + 1) % 3;
            self.dirty = true;
        } else if buttons.is_pressed(input::Buttons::Back) {
            self.full_refresh = !self.full_refresh;
            self.dirty = true;
        } else if buttons.is_pressed(input::Buttons::Confirm) {
            self.dirty = true;
        }
    }

    pub fn draw(&mut self, display: &mut impl crate::display::Display) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        match self.screen {
            0 => self.draw_shapes(display),
            1 => self.draw_image(display),
            2 => self.draw_grayscale(display),
            _ => unreachable!(),
        }
        self.full_refresh = false;
    }

    pub fn draw_image(&mut self, display: &mut impl crate::display::Display) {
        self.display_buffers
            .get_active_buffer_mut()
            .copy_from_slice(&test_image::TEST_IMAGE);
        display.display(self.display_buffers, if self.full_refresh { RefreshMode::Full } else { RefreshMode::Fast });
        display.copy_grayscale_buffers(&test_image::TEST_IMAGE_LSB, &test_image::TEST_IMAGE_MSB);
        display.display_grayscale();
    }

    pub fn draw_shapes(&mut self, display: &mut impl crate::display::Display) {
        // Clear and redraw with new rotation
        self.display_buffers.clear(BinaryColor::On).ok();

        // Get the current display size (changes with rotation)
        let size = self.display_buffers.size() - Size::new(20, 20);

        // Draw a border rectangle that fits the rotated display
        Rectangle::new(Point::new(10, 10), size)
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::Off, 2))
            .draw(self.display_buffers)
            .ok();

        // Draw some circles
        Circle::new(Point::new(100, 100), 80)
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::Off, 3))
            .draw(self.display_buffers)
            .ok();

        Circle::new(Point::new(200, 100), 60)
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(self.display_buffers)
            .ok();

        // Draw text
        let text_style = MonoTextStyle::new(&FONT_10X20, BinaryColor::Off);
        Text::new("Hello from rust", Point::new(20, 30), text_style)
            .draw(self.display_buffers)
            .ok();

        display.display(self.display_buffers, if self.full_refresh { RefreshMode::Full } else { RefreshMode::Fast });
    }

    fn draw_grayscale(&mut self, display: &mut impl crate::display::Display) {
        self.display_buffers.clear(BinaryColor::On).ok();
        let size = self.display_buffers.size() - Size::new(20, 20);

        let width = size.width as i32 - 200;
        // Black
        Rectangle::new(Point::new(100, 50), Size::new(width as _, 100))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(self.display_buffers)
            .ok();
        Rectangle::new(Point::new(100, 150), Size::new(width as _, 100))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(self.display_buffers)
            .ok();
        Rectangle::new(Point::new(100, 250), Size::new(width as _, 100))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(self.display_buffers)
            .ok();

        display.display(self.display_buffers, if self.full_refresh { RefreshMode::Full } else { RefreshMode::Fast });

        self.display_buffers.clear(BinaryColor::Off).ok();

        // Dark Gray
        Rectangle::new(Point::new(100, 150), Size::new(width as _, 100))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(self.display_buffers)
            .ok();

        // Gray
        Rectangle::new(Point::new(100, 250), Size::new(width as _, 100))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(self.display_buffers)
            .ok();

        display.copy_to_msb(self.display_buffers.get_active_buffer());

        self.display_buffers.clear(BinaryColor::Off).ok();

        // Dark Gray
        Rectangle::new(Point::new(100, 150), Size::new(width as _, 100))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(self.display_buffers)
            .ok();

        // Light Gray
        Rectangle::new(Point::new(100, 350), Size::new(width as _, 100))
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(self.display_buffers)
            .ok();

        display.copy_to_lsb(self.display_buffers.get_active_buffer());
        display.display_grayscale();
    }
}
