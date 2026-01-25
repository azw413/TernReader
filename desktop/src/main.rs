use microreader_core::{
    application::Application,
    display::{HEIGHT, WIDTH},
    framebuffer::DisplayBuffers,
};

use crate::display::MinifbDisplay;

mod display;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Microreader desktop application started");

    let options = minifb::WindowOptions {
        borderless: false,
        title: true,
        resize: true,
        scale: minifb::Scale::X2,
        ..minifb::WindowOptions::default()
    };
    let mut window = minifb::Window::new(
        "Microreader Desktop",
        // swapped
        WIDTH,
        HEIGHT,
        options,
    )
    .unwrap_or_else(|e| {
        panic!("Unable to open window: {}", e);
    });

    window.set_target_fps(5);

    let mut display_buffers = Box::new(DisplayBuffers::default());
    let mut display = Box::new(MinifbDisplay::new(window));
    let mut application = Application::new(&mut display_buffers);

    while display.is_open() {
        display.update();
        application.update(&display.get_buttons());
        application.draw(&mut *display);
    }
}
