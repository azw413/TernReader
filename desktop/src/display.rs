use log::info;
use tern_core::{
    display::{GrayscaleMode, HEIGHT, RefreshMode, WIDTH},
    framebuffer::DisplayBuffers,
    input::{ButtonState, Buttons},
};

const BUFFER_SIZE: usize = WIDTH * HEIGHT / 8;
const DISPLAY_BUFFER_SIZE: usize = WIDTH * HEIGHT;

pub struct MinifbDisplay {
    is_grayscale: bool,
    // Simulated EInk buffers
    lsb_buffer: [u8; BUFFER_SIZE],
    msb_buffer: [u8; BUFFER_SIZE],
    // Actual display buffer
    display_buffer: [u32; DISPLAY_BUFFER_SIZE],
    window: minifb::Window,
    buttons: ButtonState,
}

#[derive(PartialEq, Eq, Debug)]
enum BlitMode {
    // Blit the active framebuffer as full black/white
    Full,
    Partial,
    // Blit the difference between LSB and MSB buffers
    Grayscale,
    // Render grayscale directly from LSB/MSB buffers
    GrayscaleOneshot,
    // Revert Greyscale to black/white
    GrayscaleRevert,
}

impl MinifbDisplay {
    pub fn new(window: minifb::Window) -> Self {
        let mut ret = Self {
            is_grayscale: false,
            lsb_buffer: [0; BUFFER_SIZE],
            msb_buffer: [0; BUFFER_SIZE],
            display_buffer: [0; DISPLAY_BUFFER_SIZE],
            window,
            buttons: ButtonState::default(),
        };

        ret.display_buffer.fill(0xFFFFFFFF);

        ret
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open() && !self.window.is_key_down(minifb::Key::Escape)
    }

    pub fn update_display(&mut self /*, window: &mut minifb::Window */) {
        self.window
            .update_with_buffer(&self.display_buffer, HEIGHT, WIDTH)
            .unwrap();
    }

    pub fn update(&mut self) {
        self.window.update();
        let mut current: u8 = 0;
        if self.window.is_key_down(minifb::Key::Left) {
            current |= 1 << (Buttons::Left as u8);
        }
        if self.window.is_key_down(minifb::Key::Right) {
            current |= 1 << (Buttons::Right as u8);
        }
        if self.window.is_key_down(minifb::Key::Up) {
            current |= 1 << (Buttons::Up as u8);
        }
        if self.window.is_key_down(minifb::Key::Down) {
            current |= 1 << (Buttons::Down as u8);
        }
        if self.window.is_key_down(minifb::Key::Enter) {
            current |= 1 << (Buttons::Confirm as u8);
        }
        if self.window.is_key_down(minifb::Key::Backspace) {
            current |= 1 << (Buttons::Back as u8);
        }
        if self.window.is_key_down(minifb::Key::P) {
            current |= 1 << (Buttons::Power as u8);
        }
        self.buttons.update(current);
    }

    pub fn get_buttons(&self) -> ButtonState {
        self.buttons
    }

    fn blit_internal(&mut self, mode: BlitMode) {
        info!("Blitting with mode: {:?}", mode);
        match mode {
            BlitMode::Full => {
                let fb = self.lsb_buffer;
                for (i, byte) in fb.iter().enumerate() {
                    for bit in 0..8 {
                        let pixel_index = i * 8 + bit;
                        let pixel_value = if (byte & (1 << (7 - bit))) != 0 {
                            0xFFFFFFFF
                        } else {
                            0xFF000000
                        };
                        self.set_portrait_pixel(pixel_index, pixel_value);
                    }
                }
            }
            BlitMode::Partial => {
                for i in 0..self.lsb_buffer.len() {
                    let curr_byte = self.lsb_buffer[i];
                    let prev_byte = self.msb_buffer[i];
                    for bit in 0..8 {
                        let current_bit = (curr_byte >> (7 - bit)) & 0x01;
                        let previous_bit = (prev_byte >> (7 - bit)) & 0x01;
                        if current_bit == previous_bit {
                            continue;
                        }
                        if current_bit == 1 {
                            let pixel_index = i * 8 + bit;
                            self.set_portrait_pixel(pixel_index, 0xFFFFFFFF);
                        } else {
                            let pixel_index = i * 8 + bit;
                            self.set_portrait_pixel(pixel_index, 0xFF000000);
                        }
                    }
                }
            }
            BlitMode::Grayscale => {
                for i in 0..self.lsb_buffer.len() {
                    let lsb_byte = self.lsb_buffer[i];
                    let msb_byte = self.msb_buffer[i];
                    for bit in 0..8 {
                        let pixel_index = i * 8 + bit;
                        let lsb_bit = (lsb_byte >> (7 - bit)) & 0x01;
                        let msb_bit = (msb_byte >> (7 - bit)) & 0x01;
                        let current_pixel = self.get_portrait_pixel(pixel_index);
                        let new_pixel = match (msb_bit, lsb_bit) {
                            (0, 0) => continue,
                            (0, 1) => current_pixel.saturating_sub(0x555555), // Black -> Dark Gray
                            (1, 0) => current_pixel.saturating_sub(0xAAAAAA), // Black -> Gray
                            (1, 1) => current_pixel.saturating_add(0x333333), // White -> Light Gray
                            _ => unreachable!(),
                        };
                        self.set_portrait_pixel(pixel_index, new_pixel);
                    }
                }
            }
            BlitMode::GrayscaleOneshot => {
                for i in 0..self.lsb_buffer.len() {
                    let lsb_byte = self.lsb_buffer[i];
                    let msb_byte = self.msb_buffer[i];
                    for bit in 0..8 {
                        let pixel_index = i * 8 + bit;
                        let lsb_bit = (lsb_byte >> (7 - bit)) & 0x01;
                        let msb_bit = (msb_byte >> (7 - bit)) & 0x01;
                        let new_pixel = match (msb_bit, lsb_bit) {
                            (0, 0) => 0xFFFFFFFF,
                            (0, 1) => 0xFFAAAAAA,
                            (1, 0) => 0xFF555555,
                            (1, 1) => 0xFF000000,
                            _ => unreachable!(),
                        };
                        self.set_portrait_pixel(pixel_index, new_pixel);
                    }
                }
            }
            BlitMode::GrayscaleRevert => {
                for i in 0..self.lsb_buffer.len() {
                    let lsb_byte = self.lsb_buffer[i];
                    let msb_byte = self.msb_buffer[i];
                    for bit in 0..8 {
                        let pixel_index = i * 8 + bit;
                        let lsb_bit = (lsb_byte >> (7 - bit)) & 0x01;
                        let msb_bit = (msb_byte >> (7 - bit)) & 0x01;
                        let current_pixel = self.get_portrait_pixel(pixel_index);
                        let new_pixel = match (msb_bit, lsb_bit) {
                            (0, 0) => continue,
                            (0, 1) => current_pixel.saturating_add(0x555555), // Dark Gray  -> Black
                            (1, 0) => current_pixel.saturating_add(0xAAAAAA), // Gray       -> Black
                            (1, 1) => current_pixel.saturating_sub(0x333333), // Light Gray -> White
                            _ => unreachable!(),
                        };
                        self.set_portrait_pixel(pixel_index, new_pixel);
                    }
                }
            }
        }
        self.update_display();
    }

    fn set_portrait_pixel(&mut self, landscape_index: usize, color: u32) {
        let x_land = (landscape_index % WIDTH) as i32;
        let y_land = (landscape_index / WIDTH) as i32;
        let x_portrait = (HEIGHT as i32 - 1) - y_land;
        let y_portrait = x_land;
        if x_portrait < 0 || y_portrait < 0 {
            return;
        }
        let x_portrait = x_portrait as usize;
        let y_portrait = y_portrait as usize;
        let idx = y_portrait * HEIGHT + x_portrait;
        if idx < self.display_buffer.len() {
            self.display_buffer[idx] = color;
        }
    }

    fn get_portrait_pixel(&self, landscape_index: usize) -> u32 {
        let x_land = (landscape_index % WIDTH) as i32;
        let y_land = (landscape_index / WIDTH) as i32;
        let x_portrait = (HEIGHT as i32 - 1) - y_land;
        let y_portrait = x_land;
        if x_portrait < 0 || y_portrait < 0 {
            return 0xFFFFFFFF;
        }
        let x_portrait = x_portrait as usize;
        let y_portrait = y_portrait as usize;
        let idx = y_portrait * HEIGHT + x_portrait;
        if idx < self.display_buffer.len() {
            self.display_buffer[idx]
        } else {
            0xFFFFFFFF
        }
    }
}

impl tern_core::display::Display for MinifbDisplay {
    fn display(&mut self, buffers: &mut DisplayBuffers, mode: RefreshMode) {
        // revert grayscale first
        if self.is_grayscale {
            self.blit_internal(BlitMode::GrayscaleRevert);
            self.is_grayscale = false;
        }

        let current = buffers.get_active_buffer();
        let previous = buffers.get_inactive_buffer();
        self.lsb_buffer.copy_from_slice(&current[..]);
        self.msb_buffer.copy_from_slice(&previous[..]);
        if mode == RefreshMode::Fast {
            self.blit_internal(BlitMode::Partial);
        } else {
            self.blit_internal(BlitMode::Full);
        }
        buffers.swap_buffers();
    }
    fn copy_to_lsb(&mut self, buffers: &[u8; BUFFER_SIZE]) {
        self.lsb_buffer.copy_from_slice(buffers);
    }
    fn copy_to_msb(&mut self, buffers: &[u8; BUFFER_SIZE]) {
        self.msb_buffer.copy_from_slice(buffers);
    }
    fn copy_grayscale_buffers(&mut self, lsb: &[u8; BUFFER_SIZE], msb: &[u8; BUFFER_SIZE]) {
        self.lsb_buffer.copy_from_slice(lsb);
        self.msb_buffer.copy_from_slice(msb);
    }
    fn display_differential_grayscale(&mut self, _turn_off_screen: bool) {
        self.is_grayscale = true;
        self.blit_internal(BlitMode::Grayscale);
    }
    fn display_absolute_grayscale(&mut self, _: GrayscaleMode) {
        self.blit_internal(BlitMode::GrayscaleOneshot);
    }
}
