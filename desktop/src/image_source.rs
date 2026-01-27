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
        name.ends_with(".png") || name.ends_with(".jpg") || name.ends_with(".jpeg")
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
        let data = fs::read(&path).map_err(|_| ImageError::Io)?;
        let image = image::load_from_memory(&data).map_err(|_| ImageError::Decode)?;
        let luma = image.to_luma8();
        Ok(ImageData {
            width: luma.width(),
            height: luma.height(),
            pixels: luma.into_raw(),
        })
    }
}
