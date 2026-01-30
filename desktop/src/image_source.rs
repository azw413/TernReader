use std::fs;
use std::path::{Path, PathBuf};

use log::error;
use trusty_core::image_viewer::{EntryKind, ImageData, ImageEntry, ImageError, ImageSource};

pub struct DesktopImageSource {
    root: PathBuf,
    trbk_pages: Option<Vec<trusty_core::trbk::TrbkPage>>,
    trbk_data: Option<Vec<u8>>,
    trbk_images: Option<Vec<trusty_core::trbk::TrbkImageInfo>>,
}

impl DesktopImageSource {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            trbk_pages: None,
            trbk_data: None,
            trbk_images: None,
        }
    }

    fn is_supported(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(".png")
            || name.ends_with(".jpg")
            || name.ends_with(".jpeg")
            || name.ends_with(".trimg")
            || name.ends_with(".tri")
            || name.ends_with(".trbk")
    }

    fn resume_path(&self) -> PathBuf {
        self.root.join(".trusty_resume")
    }

    fn load_trbk_data(
        &mut self,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<(trusty_core::trbk::TrbkBook, Vec<u8>), ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Unsupported);
        }
        let base = path.iter().fold(self.root.clone(), |acc, part| acc.join(part));
        let path = base.join(&entry.name);
        let data = fs::read(&path).map_err(|_| ImageError::Io)?;
        match trusty_core::trbk::parse_trbk(&data) {
            Ok(book) => Ok((book, data)),
            Err(err) => {
                log_trbk_header(&data, &path);
                Err(err)
            }
        }
    }
}

impl ImageSource for DesktopImageSource {
    fn refresh(&mut self, path: &[String]) -> Result<Vec<ImageEntry>, ImageError> {
        let mut entries = Vec::new();
        let dir_path = path.iter().fold(self.root.clone(), |acc, part| acc.join(part));
        let read_dir = match fs::read_dir(&dir_path) {
            Ok(read_dir) => read_dir,
            Err(_) => return Ok(entries),
        };
        for entry in read_dir {
            let entry = entry.map_err(|_| ImageError::Io)?;
            let file_type = entry.file_type().map_err(|_| ImageError::Io)?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".trusty_resume" {
                continue;
            }
            if file_type.is_dir() {
                entries.push(ImageEntry {
                    name,
                    kind: EntryKind::Dir,
                });
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            if Self::is_supported(&name) {
                entries.push(ImageEntry {
                    name,
                    kind: EntryKind::File,
                });
            }
        }
        entries.sort_by(|a, b| {
            match (a.kind, b.kind) {
                (EntryKind::Dir, EntryKind::File) => std::cmp::Ordering::Less,
                (EntryKind::File, EntryKind::Dir) => std::cmp::Ordering::Greater,
                _ => a.name.cmp(&b.name),
            }
        });
        Ok(entries)
    }

    fn load(&mut self, path: &[String], entry: &ImageEntry) -> Result<ImageData, ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Unsupported);
        }
        let base = path.iter().fold(self.root.clone(), |acc, part| acc.join(part));
        let path = base.join(&entry.name);
        let lower = entry.name.to_ascii_lowercase();
        if lower.ends_with(".trbk") {
            return Err(ImageError::Unsupported);
        }
        if lower.ends_with(".trimg") || lower.ends_with(".tri") {
            let data = fs::read(&path).map_err(|_| ImageError::Io)?;
            return parse_trimg(&data);
        }

        let data = fs::read(&path).map_err(|_| ImageError::Io)?;
        let image = image::load_from_memory(&data).map_err(|_| ImageError::Decode)?;
        let luma = image.to_luma8();
        Ok(ImageData::Gray8 {
            width: luma.width(),
            height: luma.height(),
            pixels: luma.into_raw(),
        })
    }

    fn save_resume(&mut self, name: Option<&str>) {
        let path = self.resume_path();
        if let Some(name) = name {
            let _ = fs::write(path, name.as_bytes());
        } else {
            let _ = fs::remove_file(path);
        }
    }

    fn load_resume(&mut self) -> Option<String> {
        let path = self.resume_path();
        let data = fs::read(path).ok()?;
        let name = String::from_utf8_lossy(&data).trim().to_string();
        if name.is_empty() {
            None
        } else {
            Some(name)
        }
    }

    fn load_trbk(
        &mut self,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<trusty_core::trbk::TrbkBook, ImageError> {
        let (book, _) = self.load_trbk_data(path, entry)?;
        Ok(book)
    }

    fn open_trbk(
        &mut self,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<trusty_core::trbk::TrbkBookInfo, ImageError> {
        let (book, data) = self.load_trbk_data(path, entry)?;
        let info = book.info();
        self.trbk_pages = Some(book.pages);
        self.trbk_images = Some(info.images.clone());
        self.trbk_data = Some(data);
        Ok(info)
    }

    fn trbk_page(&mut self, page_index: usize) -> Result<trusty_core::trbk::TrbkPage, ImageError> {
        let Some(pages) = self.trbk_pages.as_ref() else {
            return Err(ImageError::Decode);
        };
        pages
            .get(page_index)
            .cloned()
            .ok_or(ImageError::Decode)
    }

    fn trbk_image(&mut self, image_index: usize) -> Result<ImageData, ImageError> {
        let Some(images) = self.trbk_images.as_ref() else {
            return Err(ImageError::Decode);
        };
        let Some(data) = self.trbk_data.as_ref() else {
            return Err(ImageError::Decode);
        };
        let image = images.get(image_index).ok_or(ImageError::Decode)?;
        let start = image.data_offset as usize;
        let end = start + image.data_len as usize;
        if end > data.len() {
            return Err(ImageError::Decode);
        }
        parse_trimg(&data[start..end])
    }

    fn close_trbk(&mut self) {
        self.trbk_pages = None;
        self.trbk_data = None;
        self.trbk_images = None;
    }
}

fn log_trbk_header(data: &[u8], path: &Path) {
    if data.len() < 8 {
        error!(
            "TRBK parse failed: file {} too small ({} bytes)",
            path.display(),
            data.len()
        );
        return;
    }
    if &data[0..4] != b"TRBK" {
        error!(
            "TRBK parse failed: file {} missing magic (len={})",
            path.display(),
            data.len()
        );
        return;
    }
    let version = data[4];
    let header_size = u16::from_le_bytes([data[6], data[7]]) as usize;
    let page_count = if data.len() >= 0x10 {
        u32::from_le_bytes([data[0x0C], data[0x0D], data[0x0E], data[0x0F]])
    } else {
        0
    };
    let page_lut_offset = if data.len() >= 0x18 {
        u32::from_le_bytes([data[0x14], data[0x15], data[0x16], data[0x17]])
    } else {
        0
    };
    let page_data_offset = if data.len() >= 0x20 {
        u32::from_le_bytes([data[0x1C], data[0x1D], data[0x1E], data[0x1F]])
    } else {
        0
    };
    let glyph_count = if data.len() >= 0x2C {
        u32::from_le_bytes([data[0x28], data[0x29], data[0x2A], data[0x2B]])
    } else {
        0
    };
    let glyph_table_offset = if data.len() >= 0x30 {
        u32::from_le_bytes([data[0x2C], data[0x2D], data[0x2E], data[0x2F]])
    } else {
        0
    };
    error!(
        "TRBK parse failed: {} ver={} len={} header={} pages={} page_lut={} page_data={} glyphs={} glyph_off={}",
        path.display(),
        version,
        data.len(),
        header_size,
        page_count,
        page_lut_offset,
        page_data_offset,
        glyph_count,
        glyph_table_offset
    );
}

fn parse_trimg(data: &[u8]) -> Result<ImageData, ImageError> {
    if data.len() < 16 || &data[0..4] != b"TRIM" {
        return Err(ImageError::Decode);
    }
    let version = data[4];
    let format = data[5];
    let width = u16::from_le_bytes([data[6], data[7]]) as u32;
    let height = u16::from_le_bytes([data[8], data[9]]) as u32;
    let payload = &data[16..];
    let expected = ((width as usize * height as usize) + 7) / 8;
    match (version, format) {
        (1, 1) => {
            if payload.len() != expected {
                return Err(ImageError::Decode);
            }
            Ok(ImageData::Mono1 {
                width,
                height,
                bits: payload.to_vec(),
            })
        }
        (2, 2) => {
            if payload.len() != expected * 2 {
                return Err(ImageError::Decode);
            }
            let (lsb, msb) = payload.split_at(expected);
            Ok(ImageData::Gray2Planes {
                width,
                height,
                lsb: lsb.to_vec(),
                msb: msb.to_vec(),
            })
        }
        _ => Err(ImageError::Unsupported),
    }
}
