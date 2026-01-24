#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

pub mod eink_display;
pub mod input;

use crate::eink_display::{EInkDisplay};
use crate::input::*;
use alloc::boxed::Box;
use microreader_core::application::Application;
use microreader_core::display::{Display, RefreshMode};
use microreader_core::framebuffer::DisplayBuffers;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::main;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig};
use esp_hal::spi::Mode;
use log::info;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 0x10000);
    esp_alloc::heap_allocator!(size: 310000);

    let stats = esp_alloc::HEAP.stats();
    info!("Heap initialized");
    info!("Size: {} bytes", stats.size);
    info!("Used: {} bytes", stats.current_usage);

    info!("Setting up GPIO pins");
    let cs = Output::new(peripherals.GPIO21, Level::High, OutputConfig::default());
    let dc = Output::new(peripherals.GPIO4, Level::High, OutputConfig::default());
    let busy = Input::new(peripherals.GPIO6, InputConfig::default());
    let rst = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());

    info!("Initializing SPI for E-Ink Display");
    let spi_cfg = Config::default()
        .with_frequency(Rate::from_mhz(40))
        .with_mode(Mode::_0);
    let spi = Spi::new(peripherals.SPI2, spi_cfg)
        .expect("Failed to create SPI")
        .with_sck(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO10);

    let delay = Delay::new();

    info!("SPI initialized");

    let mut display_buffers = Box::new(DisplayBuffers::new());

    // Create E-Ink Display instance
    info!("Creating E-Ink Display driver");
    let mut display = EInkDisplay::new(
        spi,
        cs,
        dc,
        rst,
        busy,
        delay
    )
    .expect("Failed to create E-Ink Display");

    // Initialize the display
    display.begin().expect("Failed to initialize display");

    info!("Clearing screen");
    display.display(&mut *display_buffers, RefreshMode::Full);

    let mut application = Application::new(&mut *display_buffers);
    let mut button_state = GpioButtonState::new(peripherals.GPIO1, peripherals.GPIO2, peripherals.GPIO3, peripherals.ADC1);
    
    info!("Display complete! Starting rotation demo...");

    loop {
        delay.delay_millis(10);

        button_state.update();
        let buttons = button_state.get_buttons();
        application.update(&buttons);
        application.draw(&mut display);
    }
}
