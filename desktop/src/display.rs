use microreader_core::{
    display::{HEIGHT, RefreshMode, WIDTH},
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

#[derive(PartialEq, Eq)]
enum BlitMode {
    // Blit the active framebuffer as full black/white
    Full,
    // Blit the difference between LSB and MSB buffers
    Greyscale,
    // Revert Greyscale to black/white
    GreyscaleRevert,
}

impl MinifbDisplay {
    pub fn new(window: minifb::Window) -> Self {
        Self {
            is_grayscale: false,
            lsb_buffer: [0; BUFFER_SIZE],
            msb_buffer: [0; BUFFER_SIZE],
            display_buffer: [0; DISPLAY_BUFFER_SIZE],
            window,
            buttons: ButtonState::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open() && !self.window.is_key_down(minifb::Key::Escape)
    }

    pub fn update_display(&mut self /*, window: &mut minifb::Window */) {
        self.window
            .update_with_buffer(&self.display_buffer, WIDTH, HEIGHT)
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
        if self.window.is_key_down(minifb::Key::Escape) {
            current |= 1 << (Buttons::Back as u8);
        }
        self.buttons.update(current);
    }

    pub fn get_buttons(&self) -> ButtonState {
        self.buttons
    }

    fn blit_internal(&mut self, mode: BlitMode) {
        if mode == BlitMode::Full {
            let fb = self.lsb_buffer;
            for (i, byte) in fb.iter().enumerate() {
                for bit in 0..8 {
                    let pixel_index = i * 8 + bit;
                    let pixel_value = if (byte & (1 << (7 - bit))) != 0 {
                        0xFFFFFFFF
                    } else {
                        0xFF000000
                    };
                    self.display_buffer[pixel_index] = pixel_value;
                }
            }
        } else {
            for i in 0..self.lsb_buffer.len() {
                let lsb_byte = self.lsb_buffer[i];
                let msb_byte = self.msb_buffer[i];
                for bit in 0..8 {
                    let pixel_index = i * 8 + bit;
                    let lsb_bit = (lsb_byte >> (7 - bit)) & 0x01;
                    let msb_bit = (msb_byte >> (7 - bit)) & 0x01;
                    let pixel_value = match (msb_bit, lsb_bit) {
                        (0, 0) => continue,   // White
                        (0, 1) => 0xFFAAAAAA, // Light Gray
                        (1, 0) => 0xFF555555, // Gray
                        (1, 1) => 0xFF222222, // Dark Gray
                        _ => panic!("????"),  // Fallback to white
                    };
                    if mode == BlitMode::Greyscale {
                        self.display_buffer[pixel_index] = pixel_value;
                    } else {
                        self.display_buffer[pixel_index] &= !pixel_value;
                    }
                }
            }
        }
        self.update_display();
    }
}

impl microreader_core::display::Display for MinifbDisplay {
    fn display(&mut self, buffers: &mut DisplayBuffers, _mode: RefreshMode) {
        // revert grayscale first
        if self.is_grayscale {
            self.blit_internal(BlitMode::GreyscaleRevert);
            self.is_grayscale = false;
        }

        let current = buffers.get_active_buffer();
        let previous = buffers.get_inactive_buffer();
        self.lsb_buffer.copy_from_slice(&current[..]);
        self.msb_buffer.copy_from_slice(&previous[..]);
        self.blit_internal(BlitMode::Full);
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
    fn display_grayscale(&mut self) {
        self.is_grayscale = true;
        self.blit_internal(BlitMode::Greyscale);
    }
}
