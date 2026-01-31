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
    fn save_book_positions(&mut self, _entries: &[(String, usize)]) {}
    fn load_book_positions(&mut self) -> Vec<(String, usize)> {
        Vec::new()
    }
    fn save_recent_entries(&mut self, _entries: &[String]) {}
    fn load_recent_entries(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn load_thumbnail(&mut self, _key: &str) -> Option<ImageData> {
        None
    }
    fn save_thumbnail(&mut self, _key: &str, _image: &ImageData) {}
    fn load_thumbnail_title(&mut self, _key: &str) -> Option<String> {
        None
    }
    fn save_thumbnail_title(&mut self, _key: &str, _title: &str) {}
}
