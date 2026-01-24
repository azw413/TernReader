use microreader_core::{
    application::Application,
    display::{HEIGHT, WIDTH}, framebuffer::DisplayBuffers,
};

use crate::display::MinifbDisplay;

mod display;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("Microreader desktop application started");

    let mut window = minifb::Window::new(
        "Microreader Desktop",
        // swapped
        WIDTH as usize,
        HEIGHT as usize,
        minifb::WindowOptions::default(),
    )
    .unwrap_or_else(|e| {
        panic!("Unable to open window: {}", e);
    });

    window.set_target_fps(60);

    let mut display_buffers = Box::new(DisplayBuffers::new());
    let mut display = Box::new(MinifbDisplay::new(window));
    let mut application = Application::new(&mut *display_buffers);

    while display.is_open() {
        display.update();
        application.update(&display.get_buttons());
        application.draw(&mut *display);
    }
}
