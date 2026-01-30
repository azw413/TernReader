extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use core_io::{Read, Seek, SeekFrom, Write};
use fatfs::{FileSystem, FsOptions};
use trusty_core::image_viewer::{EntryKind, ImageData, ImageEntry, ImageError, ImageSource};

use crate::sd_io::{detect_fat_partition, SdCardIo};

pub struct SdImageSource<D>
where
    D: embedded_sdmmc::BlockDevice,
    D::Error: core::fmt::Debug,
{
    sdcard: D,
    trbk: Option<TrbkStream>,
}

struct TrbkStream {
    path: Vec<String>,
    name: String,
    page_offsets: Vec<u32>,
    page_data_offset: u32,
    glyph_table_offset: u32,
    info: trusty_core::trbk::TrbkBookInfo,
}

impl<D> SdImageSource<D>
where
    D: embedded_sdmmc::BlockDevice,
    D::Error: core::fmt::Debug,
{
    pub fn new(sdcard: D) -> Self {
        Self { sdcard, trbk: None }
    }

    fn is_supported(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(".tri") || name.ends_with(".trbk") || name.ends_with(".epub") || name.ends_with(".epb")
    }

    fn resume_filename() -> &'static str {
        ".trusty_resume"
    }

    fn open_fs(&self) -> Result<FileSystem<SdCardIo<'_, D>>, ImageError> {
        let base_lba = detect_fat_partition(&self.sdcard).map_err(|_| ImageError::Io)?;
        let io = SdCardIo::new(&self.sdcard, base_lba).map_err(|_| ImageError::Io)?;
        FileSystem::new(io, FsOptions::new()).map_err(|_| ImageError::Io)
    }

}

fn read_exact<R: Read>(reader: &mut R, mut buf: &mut [u8]) -> Result<(), ImageError> {
    while !buf.is_empty() {
        let read = reader.read(buf).map_err(|_| ImageError::Io)?;
        if read == 0 {
            return Err(ImageError::Decode);
        }
        let tmp = buf;
        buf = &mut tmp[read..];
    }
    Ok(())
}

fn read_u16_le(data: &[u8], offset: usize) -> Result<u16, ImageError> {
    if offset + 2 > data.len() {
        return Err(ImageError::Decode);
    }
    Ok(u16::from_le_bytes([data[offset], data[offset + 1]]))
}

fn read_i16_le(data: &[u8], offset: usize) -> Result<i16, ImageError> {
    if offset + 2 > data.len() {
        return Err(ImageError::Decode);
    }
    Ok(i16::from_le_bytes([data[offset], data[offset + 1]]))
}

fn read_u32_le(data: &[u8], offset: usize) -> Result<u32, ImageError> {
    if offset + 4 > data.len() {
        return Err(ImageError::Decode);
    }
    Ok(u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ]))
}

fn read_trimg_from_file<R: Read>(reader: &mut R, len: usize) -> Result<ImageData, ImageError> {
    if len < 16 {
        return Err(ImageError::Decode);
    }
    let mut header = [0u8; 16];
    read_exact(reader, &mut header)?;
    if &header[0..4] != b"TRIM" {
        return Err(ImageError::Unsupported);
    }
    if header[4] != 1 || header[5] != 1 {
        return Err(ImageError::Unsupported);
    }
    let width = u16::from_le_bytes([header[6], header[7]]) as u32;
    let height = u16::from_le_bytes([header[8], header[9]]) as u32;
    let expected = ((width as usize * height as usize) + 7) / 8;
    if 16 + expected != len {
        return Err(ImageError::Decode);
    }

    let mut bits = Vec::new();
    if bits.try_reserve(expected).is_err() {
        return Err(ImageError::Message(
            "Not enough memory for image buffer.".into(),
        ));
    }
    let mut buffer = [0u8; 512];
    while bits.len() < expected {
        let read = reader.read(&mut buffer).map_err(|_| ImageError::Io)?;
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

    Ok(ImageData::Mono1 { width, height, bits })
}

fn read_string(data: &[u8], cursor: &mut usize) -> Result<String, ImageError> {
    let len = read_u32_le(data, *cursor)? as usize;
    *cursor += 4;
    if *cursor + len > data.len() {
        return Err(ImageError::Decode);
    }
    let value = core::str::from_utf8(&data[*cursor..*cursor + len])
        .map_err(|_| ImageError::Decode)?
        .to_string();
    *cursor += len;
    Ok(value)
}

impl<D> ImageSource for SdImageSource<D>
where
    D: embedded_sdmmc::BlockDevice,
    D::Error: core::fmt::Debug,
{
    fn refresh(&mut self, path: &[String]) -> Result<Vec<ImageEntry>, ImageError> {
        let fs = self.open_fs()?;
        let mut read_dir = fs.root_dir();
        for part in path {
            read_dir = read_dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut entries = Vec::new();
        for entry in read_dir.iter() {
            let entry = entry.map_err(|_| ImageError::Io)?;
            let name = entry.file_name();
            if name.is_empty() || name == Self::resume_filename() || name == "." || name == ".." {
                continue;
            }
            if entry.is_dir() {
                entries.push(ImageEntry {
                    name,
                    kind: EntryKind::Dir,
                });
            } else if Self::is_supported(&name) {
                entries.push(ImageEntry {
                    name,
                    kind: EntryKind::File,
                });
            }
        }

        entries.sort_by(|a, b| match (a.kind, b.kind) {
            (EntryKind::Dir, EntryKind::File) => core::cmp::Ordering::Less,
            (EntryKind::File, EntryKind::Dir) => core::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        Ok(entries)
    }

    fn load(&mut self, path: &[String], entry: &ImageEntry) -> Result<ImageData, ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Message("Select a file, not a folder.".into()));
        }
        let lower = entry.name.to_ascii_lowercase();
        if lower.ends_with(".epub") || lower.ends_with(".epb") {
            return Err(ImageError::Message("EPUB files must be converted to .trbk.".into()));
        }
        if lower.ends_with(".trbk") {
            return Err(ImageError::Unsupported);
        }

        let fs = self.open_fs()?;
        let mut dir = fs.root_dir();
        for part in path {
            dir = dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut file = dir.open_file(&entry.name).map_err(|_| ImageError::Io)?;

        const MAX_IMAGE_BYTES: usize = 120_000;
        let mut file_len = None;
        for dir_entry in dir.iter() {
            let dir_entry = dir_entry.map_err(|_| ImageError::Io)?;
            if dir_entry.file_name() == entry.name {
                file_len = Some(dir_entry.len() as usize);
                break;
            }
        }
        let Some(file_len) = file_len else {
            return Err(ImageError::Io);
        };
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
        while bits.len() < expected {
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

        Ok(ImageData::Mono1 { width, height, bits })
    }

    fn save_resume(&mut self, name: Option<&str>) {
        let fs = match self.open_fs() {
            Ok(fs) => fs,
            Err(_) => return,
        };
        let root_dir = fs.root_dir();
        let resume_name = Self::resume_filename();
        if let Some(name) = name {
            let mut file = match root_dir.open_file(resume_name) {
                Ok(file) => file,
                Err(_) => match root_dir.create_file(resume_name) {
                    Ok(file) => file,
                    Err(_) => return,
                },
            };
            let _ = file.truncate();
            let _ = file.write(name.as_bytes());
        } else {
            let _ = root_dir.remove(resume_name);
        }
    }

    fn load_resume(&mut self) -> Option<String> {
        let fs = self.open_fs().ok()?;
        let mut file = fs.root_dir().open_file(Self::resume_filename()).ok()?;
        let mut buf = [0u8; 128];
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

    fn load_trbk(
        &mut self,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<trusty_core::trbk::TrbkBook, ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Unsupported);
        }
        let fs = self.open_fs()?;
        let mut dir = fs.root_dir();
        for part in path {
            dir = dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut file = dir.open_file(&entry.name).map_err(|_| ImageError::Io)?;

        let mut file_len = None;
        for dir_entry in dir.iter() {
            let dir_entry = dir_entry.map_err(|_| ImageError::Io)?;
            if dir_entry.file_name() == entry.name {
                file_len = Some(dir_entry.len() as usize);
                break;
            }
        }
        let Some(file_len) = file_len else {
            return Err(ImageError::Io);
        };

        const MAX_BOOK_BYTES: usize = 900_000;
        if file_len < 16 || file_len > MAX_BOOK_BYTES {
            return Err(ImageError::Message(
                "Book file too large for device.".into(),
            ));
        }

        let mut data = Vec::new();
        if data.try_reserve(file_len).is_err() {
            return Err(ImageError::Message(
                "Not enough memory for book file.".into(),
            ));
        }
        let mut buffer = [0u8; 512];
        while data.len() < file_len {
            let read = file.read(&mut buffer).map_err(|_| ImageError::Io)?;
            if read == 0 {
                break;
            }
            let remaining = file_len - data.len();
            let take = read.min(remaining);
            if data.try_reserve(take).is_err() {
                return Err(ImageError::Message(
                    "Not enough memory while reading book.".into(),
                ));
            }
            data.extend_from_slice(&buffer[..take]);
        }
        if data.len() != file_len {
            return Err(ImageError::Decode);
        }

        trusty_core::trbk::parse_trbk(&data)
    }

    fn open_trbk(
        &mut self,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<trusty_core::trbk::TrbkBookInfo, ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Unsupported);
        }
        let fs = self.open_fs()?;
        let mut dir = fs.root_dir();
        for part in path {
            dir = dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut file = dir.open_file(&entry.name).map_err(|_| ImageError::Io)?;

        let mut header = [0u8; 0x30];
        read_exact(&mut file, &mut header)?;
        if &header[0..4] != b"TRBK" {
            return Err(ImageError::Decode);
        }
        let version = header[4];
        if version != 1 && version != 2 {
            return Err(ImageError::Unsupported);
        }
        let header_size = read_u16_le(&header, 0x06)? as usize;
        let screen_width = read_u16_le(&header, 0x08)?;
        let screen_height = read_u16_le(&header, 0x0A)?;
        let page_count = read_u32_le(&header, 0x0C)? as usize;
        let toc_count = read_u32_le(&header, 0x10)? as usize;
        let page_lut_offset = read_u32_le(&header, 0x14)? as u32;
        let toc_offset = read_u32_le(&header, 0x18)? as u32;
        let page_data_offset = read_u32_le(&header, 0x1C)? as u32;
        let (glyph_count, glyph_table_offset) = if version >= 2 {
            (
                read_u32_le(&header, 0x28)? as usize,
                read_u32_le(&header, 0x2C)? as u32,
            )
        } else {
            (0usize, 0u32)
        };
        let images_offset = if version >= 2 {
            read_u32_le(&header, 0x20)? as u32
        } else {
            0
        };

        if toc_count != 0 && toc_offset as usize != header_size {
            return Err(ImageError::Decode);
        }

        // Read header + metadata
        let mut header_buf = vec![0u8; header_size];
        file.seek(SeekFrom::Start(0)).map_err(|_| ImageError::Io)?;
        read_exact(&mut file, &mut header_buf)?;

        let mut cursor = if version >= 2 { 0x30 } else { 0x2C };
        let title = read_string(&header_buf, &mut cursor)?;
        let author = read_string(&header_buf, &mut cursor)?;
        let language = read_string(&header_buf, &mut cursor)?;
        let identifier = read_string(&header_buf, &mut cursor)?;
        let font_name = read_string(&header_buf, &mut cursor)?;
        let char_width = read_u16_le(&header_buf, cursor)?; cursor += 2;
        let line_height = read_u16_le(&header_buf, cursor)?; cursor += 2;
        let ascent = read_i16_le(&header_buf, cursor)?; cursor += 2;
        let margin_left = read_u16_le(&header_buf, cursor)?; cursor += 2;
        let margin_right = read_u16_le(&header_buf, cursor)?; cursor += 2;
        let margin_top = read_u16_le(&header_buf, cursor)?; cursor += 2;
        let margin_bottom = read_u16_le(&header_buf, cursor)?; cursor += 2;

        let metadata = trusty_core::trbk::TrbkMetadata {
            title,
            author,
            language,
            identifier,
            font_name,
            char_width,
            line_height,
            ascent,
            margin_left,
            margin_right,
            margin_top,
            margin_bottom,
        };

        let mut toc_entries = Vec::new();
        if toc_count > 0 {
            file.seek(SeekFrom::Start(toc_offset as u64))
                .map_err(|_| ImageError::Io)?;
            for _ in 0..toc_count {
                let mut len_buf = [0u8; 4];
                read_exact(&mut file, &mut len_buf)?;
                let title_len = u32::from_le_bytes(len_buf) as usize;
                let mut title_buf = vec![0u8; title_len];
                read_exact(&mut file, &mut title_buf)?;
                let title = core::str::from_utf8(&title_buf)
                    .map_err(|_| ImageError::Decode)?
                    .to_string();
                let mut entry_buf = [0u8; 4 + 1 + 1 + 2];
                read_exact(&mut file, &mut entry_buf)?;
                let page_index = u32::from_le_bytes([entry_buf[0], entry_buf[1], entry_buf[2], entry_buf[3]]);
                let level = entry_buf[4];
                toc_entries.push(trusty_core::trbk::TrbkTocEntry {
                    title,
                    page_index,
                    level,
                });
            }
        }

        // Page offsets
        let lut_len = page_count * 4;
        let mut page_offsets = vec![0u8; lut_len];
        file.seek(SeekFrom::Start(page_lut_offset as u64))
            .map_err(|_| ImageError::Io)?;
        read_exact(&mut file, &mut page_offsets)?;
        let mut offsets = Vec::with_capacity(page_count);
        for i in 0..page_count {
            let idx = i * 4;
            offsets.push(u32::from_le_bytes([
                page_offsets[idx],
                page_offsets[idx + 1],
                page_offsets[idx + 2],
                page_offsets[idx + 3],
            ]));
        }

        // Glyphs
        let mut glyphs = Vec::new();
        if glyph_count > 0 {
            file.seek(SeekFrom::Start(glyph_table_offset as u64))
                .map_err(|_| ImageError::Io)?;
            for _ in 0..glyph_count {
                let mut header = [0u8; 4 + 1 + 1 + 1 + 2 + 2 + 2 + 4];
                read_exact(&mut file, &mut header)?;
                let codepoint = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
                let style = header[4];
                let width = header[5];
                let height = header[6];
                let x_advance = i16::from_le_bytes([header[7], header[8]]);
                let x_offset = i16::from_le_bytes([header[9], header[10]]);
                let y_offset = i16::from_le_bytes([header[11], header[12]]);
                let bitmap_len = u32::from_le_bytes([header[13], header[14], header[15], header[16]]) as usize;
                let mut bitmap = vec![0u8; bitmap_len];
                read_exact(&mut file, &mut bitmap)?;
                glyphs.push(trusty_core::trbk::TrbkGlyph {
                    codepoint,
                    style,
                    width,
                    height,
                    x_advance,
                    x_offset,
                    y_offset,
                    bitmap,
                });
            }
        }

        let mut images = Vec::new();
        if images_offset > 0 {
            file.seek(SeekFrom::Start(images_offset as u64))
                .map_err(|_| ImageError::Io)?;
            let mut count_buf = [0u8; 4];
            read_exact(&mut file, &mut count_buf)?;
            let image_count = u32::from_le_bytes(count_buf) as usize;

            let mut first_buf = [0u8; 16];
            if image_count > 0 {
                read_exact(&mut file, &mut first_buf)?;
            }
            let table_size_16 = 4 + image_count * 16;
            let table_size_14 = 4 + image_count * 14;
            let rel_offset_16 = u32::from_le_bytes([first_buf[0], first_buf[1], first_buf[2], first_buf[3]]);
            let rel_offset_14 = u32::from_le_bytes([first_buf[0], first_buf[1], first_buf[2], first_buf[3]]);
            let entry_size = if image_count == 0 {
                16
            } else if rel_offset_16 as usize == table_size_16 {
                16
            } else if rel_offset_14 as usize == table_size_14 {
                14
            } else {
                16
            };

            let mut parse_entry = |entry_buf: &[u8]| {
                let rel_offset = u32::from_le_bytes([entry_buf[0], entry_buf[1], entry_buf[2], entry_buf[3]]);
                let data_len = u32::from_le_bytes([entry_buf[4], entry_buf[5], entry_buf[6], entry_buf[7]]);
                let width = u16::from_le_bytes([entry_buf[8], entry_buf[9]]);
                let height = u16::from_le_bytes([entry_buf[10], entry_buf[11]]);
                (rel_offset, data_len, width, height)
            };

            if image_count > 0 {
                let (rel_offset, data_len, width, height) = parse_entry(&first_buf);
                let data_offset = images_offset.saturating_add(rel_offset);
                images.push(trusty_core::trbk::TrbkImageInfo {
                    data_offset,
                    data_len,
                    width,
                    height,
                });
            }

            for _ in 1..image_count {
                if entry_size == 16 {
                    let mut entry_buf = [0u8; 16];
                    read_exact(&mut file, &mut entry_buf)?;
                    let (rel_offset, data_len, width, height) = parse_entry(&entry_buf);
                    let data_offset = images_offset.saturating_add(rel_offset);
                    images.push(trusty_core::trbk::TrbkImageInfo {
                        data_offset,
                        data_len,
                        width,
                        height,
                    });
                } else {
                    let mut entry_buf = [0u8; 14];
                    read_exact(&mut file, &mut entry_buf)?;
                    let rel_offset = u32::from_le_bytes([entry_buf[0], entry_buf[1], entry_buf[2], entry_buf[3]]);
                    let data_len = u32::from_le_bytes([entry_buf[4], entry_buf[5], entry_buf[6], entry_buf[7]]);
                    let width = u16::from_le_bytes([entry_buf[8], entry_buf[9]]);
                    let height = u16::from_le_bytes([entry_buf[10], entry_buf[11]]);
                    let data_offset = images_offset.saturating_add(rel_offset);
                    images.push(trusty_core::trbk::TrbkImageInfo {
                        data_offset,
                        data_len,
                        width,
                        height,
                    });
                }
            }
        }

        let info = trusty_core::trbk::TrbkBookInfo {
            screen_width,
            screen_height,
            page_count,
            metadata,
            glyphs: glyphs.clone(),
            toc: toc_entries,
            images,
        };

        drop(file);
        drop(dir);
        drop(fs);

        self.trbk = Some(TrbkStream {
            path: path.to_vec(),
            name: entry.name.clone(),
            page_offsets: offsets,
            page_data_offset,
            glyph_table_offset,
            info: info.clone(),
        });

        Ok(info)
    }

    fn trbk_page(&mut self, page_index: usize) -> Result<trusty_core::trbk::TrbkPage, ImageError> {
        let Some(state) = &self.trbk else {
            return Err(ImageError::Decode);
        };
        if page_index >= state.page_offsets.len() {
            return Err(ImageError::Decode);
        }
        let fs = self.open_fs()?;
        let mut dir = fs.root_dir();
        for part in &state.path {
            dir = dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut file = dir.open_file(&state.name).map_err(|_| ImageError::Io)?;

        let start = state.page_data_offset + state.page_offsets[page_index];
        let end = if page_index + 1 < state.page_offsets.len() {
            state.page_data_offset + state.page_offsets[page_index + 1]
        } else {
            state.glyph_table_offset
        };
        if end < start {
            return Err(ImageError::Decode);
        }
        let len = (end - start) as usize;
        let mut buf = vec![0u8; len];
        file.seek(SeekFrom::Start(start as u64))
            .map_err(|_| ImageError::Io)?;
        read_exact(&mut file, &mut buf)?;
        let ops = trusty_core::trbk::parse_trbk_page_ops(&buf)?;
        Ok(trusty_core::trbk::TrbkPage { ops })
    }

    fn trbk_image(&mut self, image_index: usize) -> Result<ImageData, ImageError> {
        let Some(state) = &self.trbk else {
            return Err(ImageError::Decode);
        };
        let image = state
            .info
            .images
            .get(image_index)
            .ok_or(ImageError::Decode)?;
        let fs = self.open_fs()?;
        let mut dir = fs.root_dir();
        for part in &state.path {
            dir = dir.open_dir(part).map_err(|_| ImageError::Io)?;
        }
        let mut file = dir.open_file(&state.name).map_err(|_| ImageError::Io)?;
        file.seek(SeekFrom::Start(image.data_offset as u64))
            .map_err(|_| ImageError::Io)?;
        read_trimg_from_file(&mut file, image.data_len as usize)
    }

    fn close_trbk(&mut self) {
        self.trbk = None;
    }
}
