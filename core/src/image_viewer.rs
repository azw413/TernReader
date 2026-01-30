extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
}

#[derive(Clone, Debug)]
pub struct ImageEntry {
    pub name: String,
    pub kind: EntryKind,
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
    fn refresh(&mut self, path: &[String]) -> Result<Vec<ImageEntry>, ImageError>;
    fn load(&mut self, path: &[String], entry: &ImageEntry) -> Result<ImageData, ImageError>;
    fn load_trbk(
        &mut self,
        _path: &[String],
        _entry: &ImageEntry,
    ) -> Result<crate::trbk::TrbkBook, ImageError> {
        Err(ImageError::Unsupported)
    }
    fn open_trbk(
        &mut self,
        _path: &[String],
        _entry: &ImageEntry,
    ) -> Result<crate::trbk::TrbkBookInfo, ImageError> {
        Err(ImageError::Unsupported)
    }
    fn trbk_page(&mut self, _page_index: usize) -> Result<crate::trbk::TrbkPage, ImageError> {
        Err(ImageError::Unsupported)
    }
    fn trbk_image(&mut self, _image_index: usize) -> Result<ImageData, ImageError> {
        Err(ImageError::Unsupported)
    }
    fn close_trbk(&mut self) {}
    fn sleep(&mut self) {}
    fn wake(&mut self) {}
    fn save_resume(&mut self, _name: Option<&str>) {}
    fn load_resume(&mut self) -> Option<String> {
        None
    }
}
