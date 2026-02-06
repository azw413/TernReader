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
    Gray2 {
        width: u32,
        height: u32,
        data: Vec<u8>, // concatenated planes: base | lsb | msb
    },
    Gray2Stream {
        width: u32,
        height: u32,
        key: String,
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
}

pub trait BookSource {
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
}

pub trait Gray2StreamSource {
    fn load_gray2_stream(
        &mut self,
        _key: &str,
        _width: u32,
        _height: u32,
        _rotation: crate::framebuffer::Rotation,
        _base: &mut [u8],
        _lsb: &mut [u8],
        _msb: &mut [u8],
    ) -> Result<(), ImageError> {
        Err(ImageError::Unsupported)
    }
    fn load_gray2_stream_region(
        &mut self,
        _key: &str,
        _width: u32,
        _height: u32,
        _rotation: crate::framebuffer::Rotation,
        _base: &mut [u8],
        _lsb: &mut [u8],
        _msb: &mut [u8],
        _dst_x: i32,
        _dst_y: i32,
    ) -> Result<(), ImageError> {
        Err(ImageError::Unsupported)
    }
    fn load_gray2_stream_thumbnail(
        &mut self,
        _key: &str,
        _width: u32,
        _height: u32,
        _thumb_w: u32,
        _thumb_h: u32,
    ) -> Option<ImageData> {
        None
    }
}

pub trait PersistenceSource {
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

pub trait PowerSource {
    fn sleep(&mut self) {}
    fn wake(&mut self) {}
}

pub trait AppSource:
    ImageSource + BookSource + Gray2StreamSource + PersistenceSource + PowerSource
{
}

impl<T> AppSource for T where
    T: ImageSource + BookSource + Gray2StreamSource + PersistenceSource + PowerSource
{
}
