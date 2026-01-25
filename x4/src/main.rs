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

use core::cell::RefCell;

use crate::eink_display::EInkDisplay;
use crate::input::*;
use alloc::boxed::Box;
use alloc::vec::Vec;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::RefCellDevice;
use embedded_sdmmc::{LfnBuffer, SdCard, VolumeIdx, VolumeManager};
use esp_backtrace as _;
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Input, InputConfig, Level, Output, OutputConfig};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::{UsbSerialJtag, UsbSerialJtagRx};
use log::info;
use microreader_core::application::Application;
use microreader_core::display::{Display, RefreshMode};
use microreader_core::framebuffer::DisplayBuffers;

extern crate alloc;
const MAX_BUFFER_SIZE: usize = 512;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

fn log_heap() {
    let stats = esp_alloc::HEAP.stats();
    info!("{stats}");
}

fn handle_cmd(input_bytes: &[u8]) {
    let Ok(input) = core::str::from_utf8(input_bytes).map(|cmd| cmd.trim()) else {
        return;
    };
    info!("Handling command: {input}");
    let parts = input.split_whitespace();
    let command = parts.into_iter().next().unwrap_or("");
    if command.eq_ignore_ascii_case("ls") {
        /* ... */
    } else if command.eq_ignore_ascii_case("heap") {
        log_heap();
    } else if command.eq_ignore_ascii_case("help") {
        info!("Available commands:");
        info!("  ls   - List files (not implemented)");
        info!("  heap - Show heap usage statistics");
        info!("  help - Show this help message");
    } else {
        info!("Unknown command: {}", command);
    }
}

#[embassy_executor::task]
async fn reader(mut rx: UsbSerialJtagRx<'static, Async>) {
    let mut rbuf = [0u8; MAX_BUFFER_SIZE];
    let mut cmd_buffer: Vec<u8> = Vec::new();
    cmd_buffer.reserve(0x1000);
    loop {
        let r = embedded_io_async::Read::read(&mut rx, &mut rbuf).await;
        match r {
            Ok(len) => {
                cmd_buffer.extend_from_slice(&rbuf[..len]);
                if rbuf.contains(&b'\r') || rbuf.contains(&b'\n') {
                    // Cut input off at first newline
                    let idx = cmd_buffer
                        .iter()
                        .position(|&c| c == b'\r' || c == b'\n')
                        .unwrap();
                    handle_cmd(&cmd_buffer[..idx]);
                    cmd_buffer.clear();
                }
            }
            #[allow(unreachable_patterns)]
            Err(e) => esp_println::println!("RX Error: {:?}", e),
        }
    }
}

#[allow(
    clippy::large_stack_frames,
    reason = "it's not unusual to allocate larger buffers etc. in main"
)]
#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 0x10000);
    esp_alloc::heap_allocator!(size: 300000);

    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);

    let (rx, _tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    spawner.spawn(reader(rx)).unwrap();

    info!("Heap initialized");
    log_heap();

    let delay = Delay::new();

    // Initialize shared SPI bus
    let spi_cfg = Config::default()
        .with_frequency(Rate::from_mhz(40))
        .with_mode(Mode::_0);
    let spi = Spi::new(peripherals.SPI2, spi_cfg)
        .expect("Failed to create SPI")
        .with_sck(peripherals.GPIO8)
        .with_mosi(peripherals.GPIO10)
        .with_miso(peripherals.GPIO7);
    let shared_spi = RefCell::new(spi);

    info!("Setting up GPIO pins");
    let dc = Output::new(peripherals.GPIO4, Level::High, OutputConfig::default());
    let busy = Input::new(peripherals.GPIO6, InputConfig::default());
    let rst = Output::new(peripherals.GPIO5, Level::High, OutputConfig::default());

    info!("Initializing SPI for E-Ink Display");
    let eink_cs = Output::new(peripherals.GPIO21, Level::High, OutputConfig::default());
    let eink_spi_device = RefCellDevice::new(&shared_spi, eink_cs, delay.clone())
        .expect("Failed to create SPI device");

    info!("SPI initialized");

    let mut display_buffers = Box::new(DisplayBuffers::default());

    // Create E-Ink Display instance
    info!("Creating E-Ink Display driver");
    let mut display = EInkDisplay::new(eink_spi_device, dc, rst, busy, delay);

    // Initialize the display
    display.begin().expect("Failed to initialize display");

    info!("Clearing screen");
    display.display(&mut display_buffers, RefreshMode::Full);

    let mut application = Application::new(&mut display_buffers);
    let mut button_state = GpioButtonState::new(
        peripherals.GPIO1,
        peripherals.GPIO2,
        peripherals.GPIO3,
        peripherals.ADC1,
    );

    let eink_cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
    let sdcard_spi = RefCellDevice::new(&shared_spi, eink_cs, delay.clone())
        .expect("Failed to create SPI device for SD card");

    let sdcard = SdCard::new(sdcard_spi, delay.clone());
    info!("SD Card initialized");
    if let Ok(size) = sdcard.num_bytes() {
        info!("SD Card Size: {} bytes", size);
    }

    // Open volume 0 (main partition)
    let volume_mgr = VolumeManager::new(sdcard, DummyTimeSource);
    let volume0 = volume_mgr.open_volume(VolumeIdx(0));

    // Open root directory
    let root_dir = if let Ok(ref volume) = volume0 {
        info!("Volume 0 opened");
        volume.open_root_dir().ok()
    } else {
        None
    };

    // After initializing the SD card, increase the SPI frequency
    shared_spi
        .borrow_mut()
        .apply_config(
            &Config::default()
                .with_frequency(Rate::from_mhz(2))
                .with_mode(Mode::_0),
        )
        .expect("Failed to apply the second SPI configuration");
    if let Some(root_dir) = root_dir {
        info!("Root directory opened");
        // List files in root directory
        let mut buffer = [0u8; 255];
        let mut lfn = LfnBuffer::new(&mut buffer);
        root_dir
            .iterate_dir_lfn(&mut lfn, |f, name| {
                info!(
                    "Found dir entry: {:?} ({} bytes, directory: {})",
                    name,
                    f.size,
                    f.attributes.is_directory()
                );
            })
            .ok();
    }

    info!("Display complete! Starting rotation demo...");

    loop {
        Timer::after(Duration::from_millis(10)).await;

        button_state.update();
        let buttons = button_state.get_buttons();
        application.update(&buttons);
        application.draw(&mut display);
    }
}

/// Dummy time source for embedded-sdmmc (use RTC for real timestamps)
pub struct DummyTimeSource;

impl embedded_sdmmc::TimeSource for DummyTimeSource {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}
