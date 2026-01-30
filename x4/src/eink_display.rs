//! SSD1677 E-Ink Display Driver
//!
//! This module provides a driver for the SSD1677 e-ink display controller
//! optimized for the GDEQ0426T82 4.26" 800x480 e-paper display.
//! https://github.com/CidVonHighwind/microreader/

use embedded_hal::spi::SpiDevice;
use esp_hal::{
    delay::Delay,
    gpio::{Input, Output},
};
use log::{error, info, warn};
use trusty_core::{
    display::{Display, RefreshMode},
    framebuffer::{BUFFER_SIZE, DisplayBuffers},
};

// SSD1677 Command Definitions
#[allow(dead_code)]
mod commands {
    // Initialization and reset
    pub const SOFT_RESET: u8 = 0x12;
    pub const BOOSTER_SOFT_START: u8 = 0x0C;
    pub const DRIVER_OUTPUT_CONTROL: u8 = 0x01;
    pub const BORDER_WAVEFORM: u8 = 0x3C;
    pub const TEMP_SENSOR_CONTROL: u8 = 0x18;

    // RAM and buffer management
    pub const DATA_ENTRY_MODE: u8 = 0x11;
    pub const SET_RAM_X_RANGE: u8 = 0x44;
    pub const SET_RAM_Y_RANGE: u8 = 0x45;
    pub const SET_RAM_X_COUNTER: u8 = 0x4E;
    pub const SET_RAM_Y_COUNTER: u8 = 0x4F;
    pub const WRITE_RAM_BW: u8 = 0x24;
    pub const WRITE_RAM_RED: u8 = 0x26;
    pub const AUTO_WRITE_BW_RAM: u8 = 0x46;
    pub const AUTO_WRITE_RED_RAM: u8 = 0x47;

    // Display update and refresh
    pub const DISPLAY_UPDATE_CTRL1: u8 = 0x21;
    pub const DISPLAY_UPDATE_CTRL2: u8 = 0x22;
    pub const MASTER_ACTIVATION: u8 = 0x20;

    // LUT and voltage settings
    pub const WRITE_LUT: u8 = 0x32;
    pub const GATE_VOLTAGE: u8 = 0x03;
    pub const SOURCE_VOLTAGE: u8 = 0x04;
    pub const WRITE_VCOM: u8 = 0x2C;
    pub const WRITE_TEMP: u8 = 0x1A;

    // Power management
    pub const DEEP_SLEEP: u8 = 0x10;
}

// Display update control modes
const CTRL1_NORMAL: u8 = 0x00;
const CTRL1_BYPASS_RED: u8 = 0x40;

// Data entry mode
const DATA_ENTRY_X_INC_Y_DEC: u8 = 0x01;

// Temperature sensor control
const TEMP_SENSOR_INTERNAL: u8 = 0x80;

/// Custom LUT for grayscale fast refresh (from xteink_edp reference)
const LUT_GRAYSCALE: &[u8] = &[
    // 00 black/white
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 01 light gray
    0x54, 0x54, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 10 gray
    0xAA, 0xA0, 0xA8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 11 dark gray
    0xA2, 0x22, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // L4 (VCOM)
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

    // TP/RP groups (global timing)
    0x01, 0x01, 0x01, 0x01, 0x00, // G0
    0x01, 0x01, 0x01, 0x01, 0x00, // G1
    0x01, 0x01, 0x01, 0x01, 0x00, // G2
    0x00, 0x00, 0x00, 0x00, 0x00, // G3
    0x00, 0x00, 0x00, 0x00, 0x00, // G4
    0x00, 0x00, 0x00, 0x00, 0x00, // G5
    0x00, 0x00, 0x00, 0x00, 0x00, // G6
    0x00, 0x00, 0x00, 0x00, 0x00, // G7
    0x00, 0x00, 0x00, 0x00, 0x00, // G8
    0x00, 0x00, 0x00, 0x00, 0x00, // G9

    // Frame rate
    0x8F, 0x8F, 0x8F, 0x8F, 0x8F,

    // Voltages (VGH, VSH1, VSH2, VSL, VCOM)
    0x17, 0x41, 0xA8, 0x32, 0x30,

    // Reserved
    0x00, 0x00,
];

const LUT_GRAYSCALE_REVERT: &[u8] = &[
    // 00 black/white
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 10 gray
    0x54, 0x54, 0x54, 0x54, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 01 light gray
    0xA8, 0xA8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // 11 dark gray
    0xFC, 0xFC, 0xFC, 0xFC, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // L4 (VCOM)
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,

    // TP/RP groups (global timing)
    0x01, 0x01, 0x01, 0x01, 0x01, // G0: A=1 B=1 C=1 D=1 RP=0 (4 frames)
    0x01, 0x01, 0x01, 0x01, 0x01, // G1: A=1 B=1 C=1 D=1 RP=0 (4 frames)
    0x01, 0x01, 0x01, 0x01, 0x00, // G2: A=0 B=0 C=0 D=0 RP=0 (4 frames)
    0x01, 0x01, 0x01, 0x01, 0x00, // G3: A=0 B=0 C=0 D=0 RP=0
    0x00, 0x00, 0x00, 0x00, 0x00, // G4: A=0 B=0 C=0 D=0 RP=0
    0x00, 0x00, 0x00, 0x00, 0x00, // G5: A=0 B=0 C=0 D=0 RP=0
    0x00, 0x00, 0x00, 0x00, 0x00, // G6: A=0 B=0 C=0 D=0 RP=0
    0x00, 0x00, 0x00, 0x00, 0x00, // G7: A=0 B=0 C=0 D=0 RP=0
    0x00, 0x00, 0x00, 0x00, 0x00, // G8: A=0 B=0 C=0 D=0 RP=0
    0x00, 0x00, 0x00, 0x00, 0x00, // G9: A=0 B=0 C=0 D=0 RP=0

    // Frame rate
    0x8F, 0x8F, 0x8F, 0x8F, 0x8F,

    // Voltages (VGH, VSH1, VSH2, VSL, VCOM)
    0x17, 0x41, 0xA8, 0x32, 0x30,

    // Reserved
    0x00, 0x00,
];

/// E-Ink Display driver for SSD1677
pub struct EInkDisplay<'gpio, SPI> {
    spi: SPI,
    dc: Output<'gpio>,
    rst: Output<'gpio>,
    busy: Input<'gpio>,
    delay: Delay,
    is_screen_on: bool,
    custom_lut_active: bool,
    in_grayscale_mode: bool,
    force_grayscale: bool,
}

impl<'gpio, SPI> EInkDisplay<'gpio, SPI>
where
    SPI: SpiDevice,
{
    /// Display dimensions
    pub const WIDTH: usize = 800;
    pub const HEIGHT: usize = 480;
    pub const WIDTH_BYTES: usize = Self::WIDTH / 8;
    pub const BUFFER_SIZE: usize = Self::WIDTH_BYTES * Self::HEIGHT;

    /// Create a new EInkDisplay instance
    pub fn new(
        spi: SPI,
        dc: Output<'gpio>,
        rst: Output<'gpio>,
        busy: Input<'gpio>,
        delay: Delay,
    ) -> Self {
        Self {
            spi,
            dc,
            rst,
            busy,
            delay,
            is_screen_on: false,
            custom_lut_active: false,
            in_grayscale_mode: false,
            force_grayscale: false,
        }
    }

    pub fn set_force_grayscale(&mut self, enabled: bool) {
        self.force_grayscale = enabled;
    }

    /// Initialize the display
    pub fn begin(&mut self) -> Result<(), SPI::Error> {
        info!("Initializing E-Ink Display");

        // Reset display
        self.reset_display();

        // Initialize display controller
        self.init_display_controller()?;

        info!("E-Ink Display initialized");
        Ok(())
    }

    pub fn display_gray_buffer(&mut self, turn_off_screen: bool) -> Result<(), SPI::Error> {
        warn!("Displaying grayscale buffer");
        self.in_grayscale_mode = true;
        self.set_custom_lut(LUT_GRAYSCALE)?;
        self.refresh_display(RefreshMode::Fast, turn_off_screen)?;
        self.custom_lut_active = false;
        Ok(())
    }

    fn grayscale_revert_internal(&mut self) -> Result<(), SPI::Error> {
        warn!("Reverting grayscale buffer");
        self.in_grayscale_mode = false;
        self.set_custom_lut(LUT_GRAYSCALE_REVERT)?;
        self.refresh_display(RefreshMode::Fast, false)?;
        self.custom_lut_active = false;
        Ok(())
    }

    fn set_custom_lut(&mut self, lut: &[u8]) -> Result<(), SPI::Error> {
        info!("Setting custom LUT");

        self.send_command(commands::WRITE_LUT)?;
        self.send_data(&lut[0..=104])?;

        self.send_command(commands::GATE_VOLTAGE)?;
        self.send_data(&[lut[105]])?;

        self.send_command(commands::SOURCE_VOLTAGE)?;
        self.send_data(&[lut[106], lut[107], lut[108]])?;

        self.send_command(commands::WRITE_VCOM)?;
        self.send_data(&[lut[109]])?;

        self.custom_lut_active = true;
        Ok(())
    }

    /// Enter deep sleep mode
    pub fn deep_sleep(&mut self) -> Result<(), SPI::Error> {
        info!("Entering deep sleep mode");
        self.send_command(commands::DEEP_SLEEP)?;
        self.send_data(&[0x01])?;
        Ok(())
    }

    // ========================================================================
    // Low-level display control methods
    // ========================================================================

    fn reset_display(&mut self) {
        info!("Resetting display");
        let _ = self.rst.set_high();
        self.delay.delay_millis(20);
        let _ = self.rst.set_low();
        self.delay.delay_millis(2);
        let _ = self.rst.set_high();
        self.delay.delay_millis(20);
        info!("Display reset complete");
    }

    fn send_command(&mut self, command: u8) -> Result<(), SPI::Error> {
        let _ = self.dc.set_low(); // Command mode
        self.spi.write(&[command])?;
        Ok(())
    }

    fn send_data(&mut self, data: &[u8]) -> Result<(), SPI::Error> {
        let _ = self.dc.set_high(); // Data mode
        self.spi.write(data)?;
        Ok(())
    }

    fn wait_while_busy(&mut self, comment: &str) {
        let mut iterations = 0u32;
        while self.busy.is_high() {
            self.delay.delay_millis(1);
            iterations += 1;
            if iterations > 10000 {
                error!("Timeout waiting for busy: {}", comment);
                break;
            }
        }
        info!("Wait complete: {} ({} ms)", comment, iterations);
    }

    fn init_display_controller(&mut self) -> Result<(), SPI::Error> {
        info!("Initializing SSD1677 controller");

        // Soft reset
        self.send_command(commands::SOFT_RESET)?;
        self.wait_while_busy("SOFT_RESET");

        // Temperature sensor control (internal)
        self.send_command(commands::TEMP_SENSOR_CONTROL)?;
        self.send_data(&[TEMP_SENSOR_INTERNAL])?;

        // Booster soft-start control (GDEQ0426T82 specific values)
        self.send_command(commands::BOOSTER_SOFT_START)?;
        self.send_data(&[0xAE, 0xC7, 0xC3, 0xC0, 0x40])?;

        // Driver output control: set display height (480) and scan direction
        let height: u16 = 480;
        self.send_command(commands::DRIVER_OUTPUT_CONTROL)?;
        self.send_data(&[
            ((height - 1) % 256) as u8, // gates A0..A7 (low byte)
            ((height - 1) / 256) as u8, // gates A8..A9 (high byte)
            0x02,                       // SM=1 (interlaced), TB=0
        ])?;

        // Border waveform control
        self.send_command(commands::BORDER_WAVEFORM)?;
        self.send_data(&[0x01])?;

        // Set up full screen RAM area
        self.set_ram_area(0, 0, Self::WIDTH as u16, Self::HEIGHT as u16)?;

        // Clear RAM buffers
        info!("Clearing RAM buffers");
        self.send_command(commands::AUTO_WRITE_BW_RAM)?;
        self.send_data(&[0xF7])?;
        self.wait_while_busy("AUTO_WRITE_BW_RAM");

        self.send_command(commands::AUTO_WRITE_RED_RAM)?;
        self.send_data(&[0xF7])?;
        self.wait_while_busy("AUTO_WRITE_RED_RAM");

        info!("SSD1677 controller initialized");
        Ok(())
    }

    fn set_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) -> Result<(), SPI::Error> {
        // Reverse Y coordinate (gates are reversed on this display)
        let y = Self::HEIGHT as u16 - y - h;

        // Set data entry mode (X increment, Y decrement for reversed gates)
        self.send_command(commands::DATA_ENTRY_MODE)?;
        self.send_data(&[DATA_ENTRY_X_INC_Y_DEC])?;

        // Set RAM X address range (start, end) - X is in PIXELS
        self.send_command(commands::SET_RAM_X_RANGE)?;
        self.send_data(&[
            (x % 256) as u8,           // start low byte
            (x / 256) as u8,           // start high byte
            ((x + w - 1) % 256) as u8, // end low byte
            ((x + w - 1) / 256) as u8, // end high byte
        ])?;

        // Set RAM Y address range (start, end) - Y is in PIXELS
        self.send_command(commands::SET_RAM_Y_RANGE)?;
        self.send_data(&[
            ((y + h - 1) % 256) as u8, // start low byte
            ((y + h - 1) / 256) as u8, // start high byte
            (y % 256) as u8,           // end low byte
            (y / 256) as u8,           // end high byte
        ])?;

        // Set RAM X address counter - X is in PIXELS
        self.send_command(commands::SET_RAM_X_COUNTER)?;
        self.send_data(&[
            (x % 256) as u8, // low byte
            (x / 256) as u8, // high byte
        ])?;

        // Set RAM Y address counter - Y is in PIXELS
        self.send_command(commands::SET_RAM_Y_COUNTER)?;
        self.send_data(&[
            ((y + h - 1) % 256) as u8, // low byte
            ((y + h - 1) / 256) as u8, // high byte
        ])?;

        Ok(())
    }

    fn write_ram_buffer(&mut self, ram_buffer: u8, data: &[u8]) -> Result<(), SPI::Error> {
        let buffer_name = if ram_buffer == commands::WRITE_RAM_BW {
            "BW"
        } else {
            "RED"
        };
        info!(
            "Writing frame buffer to {} RAM ({} bytes)",
            buffer_name,
            data.len()
        );

        self.send_command(ram_buffer)?;

        // Write data in chunks to avoid issues with large transfers
        const CHUNK_SIZE: usize = 4096;
        for chunk in data.chunks(CHUNK_SIZE) {
            self.send_data(chunk)?;
        }

        info!("{} RAM write complete", buffer_name);
        Ok(())
    }

    fn refresh_display(
        &mut self,
        mode: RefreshMode,
        turn_off_screen: bool,
    ) -> Result<(), SPI::Error> {
        // Configure Display Update Control 1
        self.send_command(commands::DISPLAY_UPDATE_CTRL1)?;
        let ctrl1 = match mode {
            RefreshMode::Fast => CTRL1_NORMAL,
            RefreshMode::Full | RefreshMode::Half => CTRL1_BYPASS_RED,
        };
        self.send_data(&[ctrl1])?;

        // Select appropriate display mode based on refresh type
        let mut display_mode = 0x00u8;

        // Enable counter and analog if not already on
        if !self.is_screen_on {
            self.is_screen_on = true;
            display_mode |= 0xC0; // Set CLOCK_ON and ANALOG_ON bits
        }

        // Turn off screen if requested
        if turn_off_screen {
            self.is_screen_on = false;
            display_mode |= 0x03; // Set ANALOG_OFF_PHASE and CLOCK_OFF bits
        }

        match mode {
            RefreshMode::Full => {
                display_mode |= 0x34;
            }
            RefreshMode::Half => {
                // Write high temp to the register for a faster refresh
                self.send_command(commands::WRITE_TEMP)?;
                self.send_data(&[0x5A])?;
                display_mode |= 0xD4;
            }
            RefreshMode::Fast => {
                display_mode |= if self.custom_lut_active { 0x0C } else { 0x1C };
            }
        }

        // Power on and refresh display
        let refresh_type = match mode {
            RefreshMode::Full => "full",
            RefreshMode::Half => "half",
            RefreshMode::Fast => "fast",
        };
        info!(
            "Powering on display 0x{:02X} ({} refresh)",
            display_mode, refresh_type
        );

        self.send_command(commands::DISPLAY_UPDATE_CTRL2)?;
        self.send_data(&[display_mode])?;

        self.send_command(commands::MASTER_ACTIVATION)?;

        // Wait for display to finish updating
        info!("Waiting for display refresh");
        self.wait_while_busy(refresh_type);

        Ok(())
    }
}

impl<SPI> Display for EInkDisplay<'_, SPI>
where
    SPI: SpiDevice,
{
    fn display(&mut self, buffers: &mut DisplayBuffers, mut mode: RefreshMode) {
        if !self.is_screen_on {
            // Force half refresh if screen is off
            mode = RefreshMode::Half;
        }

        if self.force_grayscale {
            // Map 1-bit buffer into 2-bit grayscale: white -> 00, black -> 11.
            buffers.copy_active_to_inactive_inverted();
            let temp = buffers.get_inactive_buffer();
            self.copy_grayscale_buffers(temp, temp);
            buffers.swap_buffers();
            self.display_gray_buffer(true).unwrap();
            return;
        }

        // If currently in grayscale mode, revert first to black/white
        if self.in_grayscale_mode {
            self.grayscale_revert_internal().unwrap();
        }

        // Set up full screen RAM area
        self.set_ram_area(0, 0, Self::WIDTH as u16, Self::HEIGHT as u16)
            .unwrap();

        // Get raw pointers to avoid borrow checker issues
        let current = buffers.get_active_buffer();
        let previous = buffers.get_inactive_buffer();

        match mode {
            RefreshMode::Full | RefreshMode::Half => {
                // For full refresh, write current buffer to both RAM buffers
                self.write_ram_buffer(commands::WRITE_RAM_BW, current)
                    .unwrap();
                self.write_ram_buffer(commands::WRITE_RAM_RED, current)
                    .unwrap();
            }
            RefreshMode::Fast => {
                // For fast refresh, write current to BW and previous to RED
                self.write_ram_buffer(commands::WRITE_RAM_BW, current)
                    .unwrap();
                self.write_ram_buffer(commands::WRITE_RAM_RED, previous)
                    .unwrap();
            }
        }

        // Swap active buffer for next time
        buffers.swap_buffers();

        // Refresh the display
        self.refresh_display(mode, false).unwrap();
    }

    fn copy_to_lsb(&mut self, buffers: &[u8; BUFFER_SIZE]) {
        self.set_ram_area(0, 0, Self::WIDTH as u16, Self::HEIGHT as u16)
            .unwrap();
        self.write_ram_buffer(commands::WRITE_RAM_BW, buffers)
            .unwrap();
    }

    fn copy_to_msb(&mut self, buffers: &[u8; BUFFER_SIZE]) {
        self.set_ram_area(0, 0, Self::WIDTH as u16, Self::HEIGHT as u16)
            .unwrap();
        self.write_ram_buffer(commands::WRITE_RAM_RED, buffers)
            .unwrap();
    }

    fn copy_grayscale_buffers(&mut self, lsb: &[u8; BUFFER_SIZE], msb: &[u8; BUFFER_SIZE]) {
        self.set_ram_area(0, 0, Self::WIDTH as u16, Self::HEIGHT as u16)
            .unwrap();
        self.write_ram_buffer(commands::WRITE_RAM_BW, lsb).unwrap();
        self.write_ram_buffer(commands::WRITE_RAM_RED, msb).unwrap();
    }

    fn display_grayscale(&mut self) {
        self.display_gray_buffer(false).unwrap();
    }
}
