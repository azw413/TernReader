use std::fs;
use std::path::{Path, PathBuf};

use trusty_core::image_viewer::{ImageData, ImageEntry, ImageError, ImageSource};

pub struct DesktopImageSource {
    root: PathBuf,
}

impl DesktopImageSource {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    fn is_supported(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(".png")
            || name.ends_with(".jpg")
            || name.ends_with(".jpeg")
            || name.ends_with(".trimg")
            || name.ends_with(".tri")
    }

    fn resume_path(&self) -> PathBuf {
        self.root.join(".trusty_resume")
    }
}

impl ImageSource for DesktopImageSource {
    fn refresh(&mut self) -> Result<Vec<ImageEntry>, ImageError> {
        let mut entries = Vec::new();
        let read_dir = match fs::read_dir(&self.root) {
            Ok(read_dir) => read_dir,
            Err(_) => return Ok(entries),
        };
        for entry in read_dir {
            let entry = entry.map_err(|_| ImageError::Io)?;
            let file_type = entry.file_type().map_err(|_| ImageError::Io)?;
            if !file_type.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if Self::is_supported(&name) {
                entries.push(ImageEntry { name });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn load(&mut self, entry: &ImageEntry) -> Result<ImageData, ImageError> {
        let path = self.root.join(&entry.name);
        let lower = entry.name.to_ascii_lowercase();
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
}

fn parse_trimg(data: &[u8]) -> Result<ImageData, ImageError> {
    if data.len() < 16 || &data[0..4] != b"TRIM" {
        return Err(ImageError::Decode);
    }
    if data[4] != 1 || data[5] != 1 {
        return Err(ImageError::Unsupported);
    }
    let width = u16::from_le_bytes([data[6], data[7]]) as u32;
    let height = u16::from_le_bytes([data[8], data[9]]) as u32;
    let payload = &data[16..];
    let expected = ((width as usize * height as usize) + 7) / 8;
    if payload.len() != expected {
        return Err(ImageError::Decode);
    }
    Ok(ImageData::Mono1 {
        width,
        height,
        bits: payload.to_vec(),
    })
}
