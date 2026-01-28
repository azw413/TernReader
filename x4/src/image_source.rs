extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_sdmmc::{Mode, VolumeIdx, VolumeManager};
use log::info;
use trusty_core::image_viewer::{ImageData, ImageEntry, ImageError, ImageSource};

pub struct SdImageSource<D, T, const MAX_DIRS: usize = 4, const MAX_FILES: usize = 4, const MAX_VOLUMES: usize = 1>
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
    <D as embedded_sdmmc::BlockDevice>::Error: core::fmt::Debug,
{
    volume_mgr: VolumeManager<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>,
}

impl<D, T, const MAX_DIRS: usize, const MAX_FILES: usize, const MAX_VOLUMES: usize>
    SdImageSource<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
    <D as embedded_sdmmc::BlockDevice>::Error: core::fmt::Debug,
{
    pub fn new(volume_mgr: VolumeManager<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>) -> Self {
        Self { volume_mgr }
    }

    fn is_supported(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(".tri")
    }

    fn resume_filename() -> &'static str {
        "RESUME.TXT"
    }

}

impl<D, T, const MAX_DIRS: usize, const MAX_FILES: usize, const MAX_VOLUMES: usize> ImageSource
    for SdImageSource<D, T, MAX_DIRS, MAX_FILES, MAX_VOLUMES>
where
    D: embedded_sdmmc::BlockDevice,
    T: embedded_sdmmc::TimeSource,
    <D as embedded_sdmmc::BlockDevice>::Error: core::fmt::Debug,
{
    fn refresh(&mut self) -> Result<Vec<ImageEntry>, ImageError> {
        let volume = self
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| ImageError::Io)?;
        let root_dir = volume.open_root_dir().map_err(|_| ImageError::Io)?;
        let images_dir = match root_dir.open_dir("IMAGES") {
            Ok(dir) => dir,
            Err(_) => {
                info!("No /images directory found.");
                return Ok(Vec::new());
            }
        };

        let mut entries = Vec::new();
        images_dir
            .iterate_dir(|entry| {
                if entry.attributes.is_directory() {
                    return;
                }
                let filename = entry.name.to_string();
                if Self::is_supported(&filename) {
                    entries.push(ImageEntry { name: filename });
                }
            })
            .map_err(|_| ImageError::Io)?;

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn load(&mut self, entry: &ImageEntry) -> Result<ImageData, ImageError> {
        const MAX_IMAGE_BYTES: usize = 120_000;

        let volume = self
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| ImageError::Io)?;
        let root_dir = volume.open_root_dir().map_err(|_| ImageError::Io)?;
        let images_dir = root_dir.open_dir("IMAGES").map_err(|_| ImageError::Io)?;
        let file = images_dir
            .open_file_in_dir(entry.name.as_str(), Mode::ReadOnly)
            .map_err(|_| ImageError::Io)?;

        let file_len = file.length() as usize;
        if file_len < 16 || file_len > MAX_IMAGE_BYTES {
            return Err(ImageError::Message(
                "Image size not supported on device.".into(),
            ));
        }

        let mut header = [0u8; 16];
        let read = file.read(&mut header).map_err(|_| ImageError::Io)?;
        if read != header.len() || &header[0..4] != b"TRIM" {
            return Err(ImageError::Unsupported);
        }
        if header[4] != 1 || header[5] != 1 {
            return Err(ImageError::Unsupported);
        }
        let width = u16::from_le_bytes([header[6], header[7]]) as u32;
        let height = u16::from_le_bytes([header[8], header[9]]) as u32;
        let expected = ((width as usize * height as usize) + 7) / 8;
        if 16 + expected != file_len {
            return Err(ImageError::Decode);
        }

        let mut bits = Vec::new();
        if bits.try_reserve(expected).is_err() {
            return Err(ImageError::Message(
                "Not enough memory for image buffer.".into(),
            ));
        }
        let mut buffer = [0u8; 512];
        while !file.is_eof() && bits.len() < expected {
            let read = file.read(&mut buffer).map_err(|_| ImageError::Io)?;
            if read == 0 {
                break;
            }
            let remaining = expected - bits.len();
            let take = read.min(remaining);
            if bits.try_reserve(take).is_err() {
                return Err(ImageError::Message(
                    "Not enough memory while reading image.".into(),
                ));
            }
            bits.extend_from_slice(&buffer[..take]);
        }
        if bits.len() != expected {
            return Err(ImageError::Decode);
        }

        Ok(ImageData::Mono1 {
            width,
            height,
            bits,
        })
    }

    fn save_resume(&mut self, name: Option<&str>) {
        let volume = match self.volume_mgr.open_volume(VolumeIdx(0)) {
            Ok(volume) => volume,
            Err(_) => return,
        };
        let root_dir = match volume.open_root_dir() {
            Ok(dir) => dir,
            Err(_) => return,
        };
        let images_dir = match root_dir.open_dir("IMAGES") {
            Ok(dir) => dir,
            Err(_) => return,
        };

        let filename = Self::resume_filename();
        if let Some(name) = name {
            if let Ok(file) = images_dir.open_file_in_dir(filename, Mode::ReadWriteCreateOrTruncate) {
                let _ = file.write(name.as_bytes());
            }
        } else {
            let _ = images_dir.delete_file_in_dir(filename);
        }
    }

    fn load_resume(&mut self) -> Option<String> {
        let volume = self.volume_mgr.open_volume(VolumeIdx(0)).ok()?;
        let root_dir = volume.open_root_dir().ok()?;
        let images_dir = root_dir.open_dir("IMAGES").ok()?;
        let file = images_dir
            .open_file_in_dir(Self::resume_filename(), Mode::ReadOnly)
            .ok()?;
        let mut buf = [0u8; 64];
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            return None;
        }
        let name = core::str::from_utf8(&buf[..read]).ok()?.trim();
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }
}
