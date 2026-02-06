use tern_core::{
    application::Application,
    display::{HEIGHT, WIDTH},
    framebuffer::DisplayBuffers,
};

use crate::display::MinifbDisplay;
use crate::image_source::DesktopImageSource;

mod display;
mod image_source;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("TernReader desktop application started");

    let options = minifb::WindowOptions {
        borderless: false,
        title: true,
        resize: true,
        scale: minifb::Scale::X2,
        ..minifb::WindowOptions::default()
    };
    let mut window = minifb::Window::new(
        "TernReader Desktop",
        HEIGHT,
        WIDTH,
        options,
    )
    .unwrap_or_else(|e| {
        panic!("Unable to open window: {}", e);
    });

    window.set_target_fps(5);

    let mut display_buffers = Box::new(DisplayBuffers::default());
    let mut display = Box::new(MinifbDisplay::new(window));
    let mut image_source = DesktopImageSource::new("sdcard");
    let mut application = Application::new(&mut display_buffers, &mut image_source);
    let mut last_tick = std::time::Instant::now();

    while display.is_open() {
        display.update();
        let elapsed_ms = last_tick.elapsed().as_millis() as u32;
        last_tick = std::time::Instant::now();
        application.update(&display.get_buttons(), elapsed_ms);
        application.draw(&mut *display);
    }
}
