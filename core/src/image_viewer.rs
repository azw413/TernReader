extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct ImageEntry {
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // 8-bit grayscale, row-major
}

#[derive(Clone, Debug)]
pub enum ImageError {
    Io,
    Decode,
    Unsupported,
    Message(String),
}

pub trait ImageSource {
    fn refresh(&mut self) -> Result<Vec<ImageEntry>, ImageError>;
    fn load(&mut self, entry: &ImageEntry) -> Result<ImageData, ImageError>;
    fn sleep(&mut self) {}
    fn wake(&mut self) {}
}
