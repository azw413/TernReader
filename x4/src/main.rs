#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

pub mod eink_display;
pub mod image_source;
pub mod input;
pub mod sdspi_fs;
pub mod usb_mode;

use core::cell::RefCell;
use crate::eink_display::EInkDisplay;
use crate::image_source::SdImageSource;
use crate::input::*;
use alloc::boxed::Box;
use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};
use embedded_hal_bus::spi::RefCellDevice;
use crate::sdspi_fs::SdSpiFilesystem;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, RtcPinWithResistors};
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rtc_cntl::{Rtc, sleep::{RtcioWakeupSource, WakeupLevel}};
use esp_hal::spi::Mode;
use esp_hal::spi::master::{Config, Spi};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;
use log::info;
use tern_core::application::Application;
use tern_core::display::{Display, RefreshMode};
use tern_core::framebuffer::DisplayBuffers;
use tern_core::input::Buttons;
use usb_mode::{usb_task, UsbMode};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use static_cell::StaticCell;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

fn log_heap() {
    let stats = esp_alloc::HEAP.stats();
    info!("{stats}");
}

// NOTE: legacy serial command reader removed; USB protocol now owns the link.

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

    let (rx, tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    static USB_MODE_CELL: StaticCell<Mutex<CriticalSectionRawMutex, UsbMode>> = StaticCell::new();
    let usb_mode = USB_MODE_CELL.init(Mutex::new(UsbMode::new(4096)));
    spawner.spawn(usb_task(rx, tx, usb_mode)).ok();

    info!("Heap initialized");
    log_heap();

    let delay = Delay::new();
    let mut rtc = Rtc::new(peripherals.LPWR);

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

    let eink_cs = Output::new(peripherals.GPIO12, Level::High, OutputConfig::default());
    let sdcard_spi = RefCellDevice::new(&shared_spi, eink_cs, delay.clone())
        .expect("Failed to create SPI device for SD card");

    let sdcard = SdSpiFilesystem::new_with_volume(sdcard_spi, delay.clone())
        .expect("Failed to create SD SPI filesystem");
    info!("SD Card initialized");

    let mut image_source = SdImageSource::new(sdcard);
    let mut application = Application::new(&mut display_buffers, &mut image_source);
    let mut button_state = GpioButtonState::new(
        peripherals.GPIO1,
        peripherals.GPIO2,
        peripherals.GPIO0,
        peripherals.GPIO3,
        peripherals.ADC1,
    );
    let mut battery_timer_ms: u32 = 0;
    let initial_battery = button_state.read_battery_percent();
    application.set_battery_percent(initial_battery);

    // After initializing the SD card, increase the SPI frequency
    shared_spi
        .borrow_mut()
        .apply_config(
            &Config::default()
                .with_frequency(Rate::from_mhz(2))
                .with_mode(Mode::_0),
        )
        .expect("Failed to apply the second SPI configuration");
    info!("Display complete! Starting image viewer...");

    loop {
        Timer::after(Duration::from_millis(10)).await;

        button_state.update();
        let buttons = button_state.get_buttons();
        let usb_state = {
            let guard = usb_mode.lock().await;
            guard.state()
        };
        match usb_state {
            usb_mode::UsbModeState::Prompt => {
                application.draw_usb_modal(
                    &mut display,
                    "USB Connected",
                    "Enable USB file access?",
                    "Confirm = OK, Back = Cancel",
                );
                if buttons.is_pressed(Buttons::Confirm) {
                    let mut guard = usb_mode.lock().await;
                    guard.accept();
                } else if buttons.is_pressed(Buttons::Back) {
                    let mut guard = usb_mode.lock().await;
                    guard.reject();
                }
                continue;
            }
            usb_mode::UsbModeState::Active => {
                application.draw_usb_modal(
                    &mut display,
                    "USB File Access",
                    "USB mode active",
                    "Eject in host to exit",
                );
                continue;
            }
            usb_mode::UsbModeState::Rejected => {
                application.draw_usb_modal(
                    &mut display,
                    "USB Disabled",
                    "USB access rejected",
                    "Unplug to retry",
                );
                continue;
            }
            usb_mode::UsbModeState::Idle => {}
        }

        application.update(&buttons, 10);
        battery_timer_ms = battery_timer_ms.saturating_add(10);
        if battery_timer_ms >= 30_000 {
            battery_timer_ms = 0;
            let percent = button_state.read_battery_percent();
            application.set_battery_percent(percent);
        }
        application.draw(&mut display);
        let _ = application.take_wake_transition();
        if application.take_sleep_transition() {
            display.deep_sleep().ok();
            let mut wake_pin = unsafe { AnyPin::steal(3) };
            wake_pin.rtcio_pullup(true);
            wake_pin.rtcio_pulldown(false);
            let mut wake_pins: [(&mut dyn esp_hal::gpio::RtcPinWithResistors, WakeupLevel); 1] =
                [(&mut wake_pin, WakeupLevel::Low)];
            let rtcio = RtcioWakeupSource::new(&mut wake_pins);
            rtc.sleep_deep(&[&rtcio]);
        }
    }
}
