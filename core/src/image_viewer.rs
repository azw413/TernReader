extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct ImageEntry {
    pub name: String,
}

#[derive(Clone, Debug)]
pub enum ImageData {
    Gray8 {
        width: u32,
        height: u32,
        pixels: Vec<u8>, // 8-bit grayscale, row-major
    },
    Mono1 {
        width: u32,
        height: u32,
        bits: Vec<u8>, // 1-bit packed, row-major, MSB first
    },
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
    fn save_resume(&mut self, _name: Option<&str>) {}
    fn load_resume(&mut self) -> Option<String> {
        None
    }
}
