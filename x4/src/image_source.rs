extern crate alloc;

use alloc::format;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use embedded_io::{Read, Seek, SeekFrom, Write};
use tern_core::fs::{DirEntry, Directory, File, Filesystem, Mode};
use crate::sdspi_fs::UsbFsOps;
use tern_core::image_viewer::{
    BookSource, EntryKind, Gray2StreamSource, ImageData, ImageEntry, ImageError, ImageSource,
    PersistenceSource, PowerSource,
};

pub struct SdImageSource<F>
where
    F: Filesystem + 'static,
{
    fs: F,
    trbk: Option<TrbkStream>,
    short_names: Vec<(String, String)>,
    usb_stream: Option<Box<UsbWriteStreamState<F::File<'static>>>>,
}

pub struct UsbDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

pub trait UsbStorage {
    fn usb_list(&mut self, path: &str) -> Result<Vec<UsbDirEntry>, ImageError>;
    fn usb_read(&mut self, path: &str, offset: u64, length: u32) -> Result<Vec<u8>, ImageError>;
    fn usb_write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u32, ImageError>;
    fn usb_write_stream(
        &mut self,
        path: &str,
        offset: u64,
        data: &[u8],
        final_chunk: bool,
    ) -> Result<u32, ImageError> {
        let _ = final_chunk;
        self.usb_write(path, offset, data)
    }
    fn usb_delete(&mut self, path: &str) -> Result<(), ImageError>;
    fn usb_rmdir(&mut self, path: &str) -> Result<(), ImageError>;
    fn usb_rename(&mut self, from: &str, to: &str) -> Result<(), ImageError>;
    fn usb_mkdir(&mut self, path: &str) -> Result<(), ImageError>;
}

struct UsbWriteStreamState<FileT> {
    path: String,
    file: FileT,
    next_offset: u64,
}

struct TrbkStream {
    path: Vec<String>,
    name: String,
    short_name: Option<String>,
    page_offsets: Vec<u32>,
    page_data_offset: u32,
    glyph_table_offset: u32,
    info: Rc<tern_core::trbk::TrbkBookInfo>,
}

impl<F> SdImageSource<F>
where
    F: Filesystem + 'static,
{
    fn join_usb_path(dir: &str, name: &str) -> String {
        if dir.is_empty() || dir == "/" {
            return format!("/{}", name);
        }
        format!("{}/{}", dir.trim_end_matches('/'), name)
    }

    fn build_path(path: &[String], name: &str) -> String {
        if path.is_empty() {
            return name.to_string();
        }
        let mut parts: Vec<&str> = Vec::new();
        for part in path.iter().map(|p| p.as_str()) {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                _ => parts.push(part),
            }
        }
        match name {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(name),
        }
        parts.join("/")
    }
    fn entry_path_string(&self, path: &[String], entry: &ImageEntry) -> String {
        if path.is_empty() {
            entry.name.clone()
        } else {
            let mut parts = path.to_vec();
            parts.push(entry.name.clone());
            parts.join("/")
        }
    }

    pub fn new(fs: F) -> Self {
        Self {
            fs,
            trbk: None,
            short_names: Vec::new(),
            usb_stream: None,
        }
    }

    fn lookup_short_name(&self, name: &str) -> Option<String> {
        for (long, short) in &self.short_names {
            if long.eq_ignore_ascii_case(name) {
                return Some(short.clone());
            }
        }
        None
    }

    fn is_supported(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.ends_with(".tri")
            || name.ends_with(".trbk")
            || name.ends_with(".tbk")
            || name.ends_with(".epub")
            || name.ends_with(".epb")
    }

    fn resume_filename() -> &'static str {
        "TRRESUME"
    }

    fn resume_filename_legacy() -> &'static str {
        ".trusty_resume"
    }

    fn book_positions_filename() -> &'static str {
        "TRBOOKS"
    }

    fn book_positions_filename_legacy() -> &'static str {
        ".trusty_books"
    }

    fn recent_entries_filename() -> &'static str {
        "TRRECENT"
    }

    fn recent_entries_filename_legacy() -> &'static str {
        ".trusty_recents"
    }

    fn thumbnails_dirname() -> &'static str {
        "TRCACHE"
    }

    fn thumbnails_dirname_legacy() -> &'static str {
        ".trusty_cache"
    }

    fn thumbnail_name(key: &str) -> String {
        let hash = thumb_hash_hex(key);
        let short = &hash[..6.min(hash.len())];
        let mut name = String::from("TH");
        name.push_str(short);
        name.push_str(".TRI");
        name
    }

    fn thumbnail_title_name(key: &str) -> String {
        let hash = thumb_hash_hex(key);
        let short = &hash[..6.min(hash.len())];
        let mut name = String::from("TT");
        name.push_str(short);
        name.push_str(".TXT");
        name
    }

    fn read_resume(&self) -> Option<String> {
        let mut file = self
            .fs
            .open_file(Self::resume_filename(), Mode::Read)
            .or_else(|_| self.fs.open_file(Self::resume_filename_legacy(), Mode::Read))
            .ok()?;
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

    fn read_book_positions(&self) -> Vec<(String, usize)> {
        let mut file = match self
            .fs
            .open_file(Self::book_positions_filename(), Mode::Read)
            .or_else(|_| self.fs.open_file(Self::book_positions_filename_legacy(), Mode::Read))
        {
            Ok(file) => file,
            Err(_) => return Vec::new(),
        };
        let mut data = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            let read = match file.read(&mut buffer) {
                Ok(read) => read,
                Err(_) => return Vec::new(),
            };
            if read == 0 {
                break;
            }
            if data.try_reserve(read).is_err() {
                return Vec::new();
            }
            data.extend_from_slice(&buffer[..read]);
        }
        let text = match core::str::from_utf8(&data) {
            Ok(text) => text,
            Err(_) => return Vec::new(),
        };
        let mut entries = Vec::new();
        for line in text.lines() {
            let Some((name, page_str)) = line.split_once('\t') else {
                continue;
            };
            let name = name.trim();
            let page_str = page_str.trim();
            if name.is_empty() {
                continue;
            }
            let Ok(page) = page_str.parse::<usize>() else {
                continue;
            };
            entries.push((name.to_string(), page));
        }
        entries
    }

}

impl<F> UsbStorage for SdImageSource<F>
where
    F: Filesystem + UsbFsOps + 'static,
    for<'a> F::File<'a>: 'static,
{
    fn usb_list(&mut self, path: &str) -> Result<Vec<UsbDirEntry>, ImageError> {
        let listed = {
            let dir = self.fs.open_directory(path).map_err(|_| ImageError::Io)?;
            dir.list().map_err(|_| ImageError::Io)?
        };
        let mut out = Vec::new();
        for entry in listed {
            out.push(UsbDirEntry {
                name: entry.name().to_string(),
                is_dir: entry.is_directory(),
                size: entry.size() as u64,
            });
        }
        Ok(out)
    }

    fn usb_read(&mut self, path: &str, offset: u64, length: u32) -> Result<Vec<u8>, ImageError> {
        let mut file = self.fs.open_file(path, Mode::Read).map_err(|_| ImageError::Io)?;
        let _ = file.seek(SeekFrom::Start(offset)).map_err(|_| ImageError::Io)?;
        let mut buf = vec![0u8; length as usize];
        let read = file.read(&mut buf).map_err(|_| ImageError::Io)?;
        buf.truncate(read);
        Ok(buf)
    }

    fn usb_write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u32, ImageError> {
        let mut file = if offset == 0 {
            match self.fs.open_file(path, Mode::Write) {
                Ok(file) => file,
                Err(err) => {
                    return Err(ImageError::Message(alloc::format!("open write failed: {:?}", err)));
                }
            }
        } else {
            self.fs
                .open_file(path, Mode::ReadWrite)
                .map_err(|err| ImageError::Message(alloc::format!("open rw failed: {:?}", err)))?
        };
        let _ = file
            .seek(SeekFrom::Start(offset))
            .map_err(|err| ImageError::Message(alloc::format!("seek failed: {:?}", err)))?;
        let written = file
            .write(data)
            .map_err(|err| ImageError::Message(alloc::format!("write failed: {:?}", err)))?;
        let _ = file
            .flush()
            .map_err(|err| ImageError::Message(alloc::format!("flush failed: {:?}", err)))?;
        Ok(written as u32)
    }

    fn usb_write_stream(
        &mut self,
        path: &str,
        offset: u64,
        data: &[u8],
        final_chunk: bool,
    ) -> Result<u32, ImageError> {
        if offset == 0 {
            if let Some(mut stream) = self.usb_stream.take() {
                let _ = stream.file.flush();
            }
            let file = self
                .fs
                .open_file(path, Mode::Write)
                .map_err(|err| ImageError::Message(alloc::format!("open write failed: {:?}", err)))?;
            // SAFETY: UsbStorage is only used on device with owned file handles (FatFs).
            // We widen the lifetime to store the handle across calls.
            let file = unsafe {
                core::mem::transmute::<F::File<'_>, F::File<'static>>(file)
            };
            self.usb_stream = Some(Box::new(UsbWriteStreamState {
                path: path.to_string(),
                file,
                next_offset: 0,
            }));
        }

        let Some(stream) = self.usb_stream.as_mut() else {
            return Err(ImageError::Message("usb stream not initialized".into()));
        };
        if !stream.path.eq_ignore_ascii_case(path) {
            return Err(ImageError::Message("usb stream path mismatch".into()));
        }
        if stream.next_offset != offset {
            let _ = stream
                .file
                .seek(SeekFrom::Start(offset))
                .map_err(|err| ImageError::Message(alloc::format!("seek failed: {:?}", err)))?;
            stream.next_offset = offset;
        }
        let written = stream
            .file
            .write(data)
            .map_err(|err| ImageError::Message(alloc::format!("write failed: {:?}", err)))?;
        stream.next_offset = stream.next_offset.saturating_add(written as u64);
        if final_chunk {
            let _ = stream
                .file
                .flush()
                .map_err(|err| ImageError::Message(alloc::format!("flush failed: {:?}", err)))?;
            self.usb_stream = None;
        }
        Ok(written as u32)
    }

    fn usb_delete(&mut self, path: &str) -> Result<(), ImageError> {
        self.fs.delete_file(path).map_err(|_| ImageError::Io)?;
        self.cleanup_deleted_path_with_usb(path);
        Ok(())
    }

    fn usb_rmdir(&mut self, path: &str) -> Result<(), ImageError> {
        self.usb_delete_dir_recursive(path)?;
        self.fs.delete_file(path).map_err(|_| ImageError::Io)?;
        Ok(())
    }

    fn usb_rename(&mut self, from: &str, to: &str) -> Result<(), ImageError> {
        self.fs.rename_file(from, to).map_err(|_| ImageError::Io)
    }

    fn usb_mkdir(&mut self, path: &str) -> Result<(), ImageError> {
        self.fs.create_dir_all(path).map_err(|_| ImageError::Io)
    }
}

impl<F> SdImageSource<F>
where
    F: Filesystem,
{
    fn normalize_deleted_path(path: &str) -> String {
        path.trim_start_matches('/').to_string()
    }

    fn path_matches(entry: &str, target: &str) -> bool {
        entry.eq_ignore_ascii_case(target)
            || entry.trim_start_matches('/').eq_ignore_ascii_case(target)
    }

    fn cleanup_deleted_path_with_usb(&mut self, path: &str)
    where
        F: UsbFsOps,
    {
        let target = Self::normalize_deleted_path(path);
        if target.is_empty() {
            return;
        }
        if let Some(resume) = self.read_resume() {
            if Self::path_matches(&resume, &target) {
                self.save_resume(None);
            }
        }

        let mut recents = self.load_recent_entries();
        let old_len = recents.len();
        recents.retain(|entry| !Self::path_matches(entry, &target));
        if recents.len() != old_len {
            self.save_recent_entries(&recents);
        }

        let mut positions = self.read_book_positions();
        let old_len = positions.len();
        positions.retain(|(entry, _)| !Self::path_matches(entry, &target));
        if positions.len() != old_len {
            self.save_book_positions(&positions);
        }

        let thumb = Self::thumbnail_name(&target);
        let title = Self::thumbnail_title_name(&target);
        let cache_primary = Self::thumbnails_dirname();
        let cache_legacy = Self::thumbnails_dirname_legacy();
        let _ = self
            .fs
            .delete_file(&format!("{}/{}", cache_primary, thumb));
        let _ = self
            .fs
            .delete_file(&format!("{}/{}", cache_primary, title));
        let _ = self
            .fs
            .delete_file(&format!("{}/{}", cache_legacy, thumb));
        let _ = self
            .fs
            .delete_file(&format!("{}/{}", cache_legacy, title));
    }

    fn usb_delete_dir_recursive(&mut self, path: &str) -> Result<(), ImageError>
    where
        F: UsbFsOps,
    {
        let listed = {
            let dir = self.fs.open_directory(path).map_err(|_| ImageError::Io)?;
            dir.list().map_err(|_| ImageError::Io)?
        };
        let entries: Vec<(String, bool)> = listed
            .into_iter()
            .map(|entry| (entry.name().to_string(), entry.is_directory()))
            .collect();
        for (name, is_dir) in entries {
            if name == "." || name == ".." {
                continue;
            }
            let full_path = Self::join_usb_path(path, &name);
            if is_dir {
                self.usb_delete_dir_recursive(&full_path)?;
                let _ = self.fs.delete_file(&full_path);
            } else {
                let _ = self.fs.delete_file(&full_path);
                self.cleanup_deleted_path_with_usb(&full_path);
            }
        }
        Ok(())
    }
}

fn read_exact<R: Read + ?Sized>(reader: &mut R, mut buf: &mut [u8]) -> Result<(), ImageError> {
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

fn write_all<W: Write>(writer: &mut W, mut data: &[u8]) -> Result<(), ImageError> {
    while !data.is_empty() {
        let written = writer.write(data).map_err(|_| ImageError::Io)?;
        if written == 0 {
            return Err(ImageError::Io);
        }
        data = &data[written..];
    }
    Ok(())
}

fn thumb_hash_hex(key: &str) -> String {
    let mut hash: u32 = 0x811c9dc5;
    for b in key.as_bytes() {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    let mut out = String::new();
    for nibble in (0..8).rev() {
        let value = (hash >> (nibble * 4)) & 0xF;
        let ch = match value {
            0..=9 => (b'0' + value as u8) as char,
            _ => (b'a' + (value as u8 - 10)) as char,
        };
        out.push(ch);
    }
    out
}

fn serialize_thumbnail(image: &ImageData) -> Option<Vec<u8>> {
    let (width, height, bits, version, format) = match image {
        ImageData::Mono1 {
            width,
            height,
            bits,
        } => (*width, *height, bits.as_slice(), 1u8, 1u8),
        ImageData::Gray2 { width, height, data } => (*width, *height, data.as_slice(), 2u8, 2u8),
        _ => return None,
    };
    let expected = ((width as usize * height as usize) + 7) / 8;
    let expected_len = if version == 2 { expected * 3 } else { expected };
    if bits.len() != expected_len {
        return None;
    }
    let mut data = Vec::new();
    if data.try_reserve(16 + bits.len()).is_err() {
        return None;
    }
    data.extend_from_slice(b"TRIM");
    data.push(version);
    data.push(format);
    data.extend_from_slice(&(width as u16).to_le_bytes());
    data.extend_from_slice(&(height as u16).to_le_bytes());
    data.extend_from_slice(&[0u8; 6]);
    data.extend_from_slice(bits);
    Some(data)
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
    let width = u16::from_le_bytes([header[6], header[7]]) as u32;
    let height = u16::from_le_bytes([header[8], header[9]]) as u32;
    let plane = ((width as usize * height as usize) + 7) / 8;

    match (header[4], header[5]) {
        (1, 1) => {
            if 16 + plane != len {
                return Err(ImageError::Decode);
            }
            let mut bits = Vec::new();
            if bits.try_reserve(plane).is_err() {
                return Err(ImageError::Message(
                    "Not enough memory for image buffer.".into(),
                ));
            }
            let mut buffer = [0u8; 512];
            while bits.len() < plane {
                let read = reader.read(&mut buffer).map_err(|_| ImageError::Io)?;
                if read == 0 {
                    break;
                }
                let remaining = plane - bits.len();
                let take = read.min(remaining);
                if bits.try_reserve(take).is_err() {
                    return Err(ImageError::Message(
                        "Not enough memory while reading image.".into(),
                    ));
                }
                bits.extend_from_slice(&buffer[..take]);
            }
            if bits.len() != plane {
                return Err(ImageError::Decode);
            }
            Ok(ImageData::Mono1 { width, height, bits })
        }
        (2, 2) => {
            if 16 + plane * 3 != len {
                return Err(ImageError::Decode);
            }
            let mut data = Vec::new();
            if data.try_reserve(plane * 3).is_err() {
                return Err(ImageError::Message(
                    "Not enough memory for grayscale image.".into(),
                ));
            }
            data.resize(plane * 3, 0u8);
            read_exact(reader, &mut data)?;
            Ok(ImageData::Gray2 { width, height, data })
        }
        _ => Err(ImageError::Unsupported),
    }
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

impl<F> ImageSource for SdImageSource<F>
where
    F: Filesystem,
{
    fn refresh(&mut self, path: &[String]) -> Result<Vec<ImageEntry>, ImageError> {
        let path_str = if path.is_empty() {
            "/".to_string()
        } else {
            path.join("/")
        };
        log::info!("SD refresh dir: '{}'", path_str);
        let read_dir = match self.fs.open_directory(&path_str) {
            Ok(dir) => dir,
            Err(_) => {
                let upper = path_str.to_ascii_uppercase();
                if upper != path_str {
                    match self.fs.open_directory(&upper) {
                        Ok(dir) => dir,
                        Err(_) => {
                            log::warn!("Failed to open directory: '{}'", upper);
                            log::warn!("Failed to open directory: '{}'", path_str);
                            return Err(ImageError::Io);
                        }
                    }
                } else {
                    log::warn!("Failed to open directory: '{}'", path_str);
                    return Err(ImageError::Io);
                }
            }
        };
        let mut entries = Vec::new();
        let listed = read_dir.list().map_err(|err| {
            log::warn!("Failed to list directory '{}': {:?}", path_str, err);
            ImageError::Io
        })?;
        self.short_names.clear();
        for entry in listed {
            let name = entry.name().to_string();
            let short = entry.short_name().to_string();
            let upper = name.to_ascii_uppercase();
            let short_upper = short.to_ascii_uppercase();
            let short_is_hidden = short.starts_with('.');
            if !name.is_empty() {
                self.short_names.push((name.clone(), short));
            }
            if name.is_empty()
                || name.starts_with('.')
                || short_is_hidden
                || upper == Self::resume_filename()
                || upper == Self::resume_filename_legacy().to_ascii_uppercase()
                || upper == Self::book_positions_filename()
                || upper == Self::book_positions_filename_legacy().to_ascii_uppercase()
                || upper == Self::recent_entries_filename()
                || upper == Self::recent_entries_filename_legacy().to_ascii_uppercase()
                || upper == Self::thumbnails_dirname()
                || upper == Self::thumbnails_dirname_legacy().to_ascii_uppercase()
                || short_upper == Self::resume_filename()
                || short_upper == Self::book_positions_filename()
                || short_upper == Self::recent_entries_filename()
                || short_upper == Self::thumbnails_dirname()
            {
                continue;
            }
            if entry.is_directory() {
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
        if lower.ends_with(".trbk") || lower.ends_with(".tbk") {
            return Err(ImageError::Unsupported);
        }

        let file_path = Self::build_path(path, &entry.name);
        let mut file = self
            .fs
            .open_file(&file_path, Mode::Read)
            .map_err(|_| ImageError::Io)?;

        const MAX_IMAGE_BYTES: usize = 200_000;
        let file_len = file.size();
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
        let width = u16::from_le_bytes([header[6], header[7]]) as u32;
        let height = u16::from_le_bytes([header[8], header[9]]) as u32;
        let plane = ((width as usize * height as usize) + 7) / 8;
        match (header[4], header[5]) {
            (1, 1) => {
                if 16 + plane != file_len {
                    return Err(ImageError::Decode);
                }
                let mut bits = Vec::new();
                if bits.try_reserve(plane).is_err() {
                    return Err(ImageError::Message(
                        "Not enough memory for image buffer.".into(),
                    ));
                }
                let mut buffer = [0u8; 512];
                while bits.len() < plane {
                    let read = file.read(&mut buffer).map_err(|_| ImageError::Io)?;
                    if read == 0 {
                        break;
                    }
                    let remaining = plane - bits.len();
                    let take = read.min(remaining);
                    if bits.try_reserve(take).is_err() {
                        return Err(ImageError::Message(
                            "Not enough memory while reading image.".into(),
                        ));
                    }
                    bits.extend_from_slice(&buffer[..take]);
                }
                if bits.len() != plane {
                    return Err(ImageError::Decode);
                }
                Ok(ImageData::Mono1 { width, height, bits })
            }
            (2, 2) => {
                if 16 + plane * 3 != file_len {
                    return Err(ImageError::Decode);
                }
                let key = self.entry_path_string(path, entry);
                Ok(ImageData::Gray2Stream { width, height, key })
            }
            _ => Err(ImageError::Unsupported),
        }
    }

}

impl<F> PersistenceSource for SdImageSource<F>
where
    F: Filesystem,
{
    fn save_resume(&mut self, name: Option<&str>) {
        let resume_name = Self::resume_filename();
        if let Some(name) = name {
            log::info!("Saving resume state: {}", name);
            let mut file = match self.fs.open_file(resume_name, Mode::Write) {
                Ok(file) => file,
                Err(_) => return,
            };
            let mut written = 0usize;
            let bytes = name.as_bytes();
            while written < bytes.len() {
                match file.write(&bytes[written..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => written += n,
                }
            }
            let _ = file.flush();
        } else {
            let _ = self.fs.open_file(resume_name, Mode::Write);
        }
    }

    fn load_resume(&mut self) -> Option<String> {
        self.read_resume()
    }

    fn save_book_positions(&mut self, entries: &[(String, usize)]) {
        let positions_name = Self::book_positions_filename();
        if entries.is_empty() {
            return;
        }
        let mut file = match self.fs.open_file(positions_name, Mode::Write) {
            Ok(file) => file,
            Err(_) => return,
        };
        for (name, page) in entries {
            let mut line = String::new();
            line.push_str(name);
            line.push('\t');
            line.push_str(&page.to_string());
            line.push('\n');
            if write_all(&mut file, line.as_bytes()).is_err() {
                return;
            }
        }
        let _ = file.flush();
    }

    fn load_book_positions(&mut self) -> Vec<(String, usize)> {
        self.read_book_positions()
    }

    fn save_recent_entries(&mut self, entries: &[String]) {
        let name = Self::recent_entries_filename();
        if entries.is_empty() {
            return;
        }
        log::info!("Saving recent entries: {} -> {}", entries.len(), name);
        let mut file = match self.fs.open_file(name, Mode::Write) {
            Ok(file) => file,
            Err(err) => {
                log::warn!("Failed to open recent entries file {}: {:?}", name, err);
                return;
            }
        };
        for entry in entries {
            if write_all(&mut file, entry.as_bytes()).is_err() {
                log::warn!("Failed to write recent entry to {}", name);
                return;
            }
            if write_all(&mut file, b"\n").is_err() {
                log::warn!("Failed to write recent entry newline to {}", name);
                return;
            }
        }
        let _ = file.flush();
    }

    fn load_recent_entries(&mut self) -> Vec<String> {
        let mut file = match self
            .fs
            .open_file(Self::recent_entries_filename(), Mode::Read)
            .or_else(|_| self.fs.open_file(Self::recent_entries_filename_legacy(), Mode::Read))
        {
            Ok(file) => file,
            Err(err) => {
                log::info!("No recent entries file: {:?}", err);
                if let Ok(mut file) = self.fs.open_file(Self::recent_entries_filename(), Mode::Write) {
                    let _ = file.flush();
                }
                return Vec::new();
            }
        };
        let mut data = Vec::new();
        let mut buffer = [0u8; 256];
        loop {
            let read = match file.read(&mut buffer) {
                Ok(read) => read,
                Err(_) => return Vec::new(),
            };
            if read == 0 {
                break;
            }
            if data.try_reserve(read).is_err() {
                return Vec::new();
            }
            data.extend_from_slice(&buffer[..read]);
        }
        let text = match core::str::from_utf8(&data) {
            Ok(text) => text,
            Err(_) => return Vec::new(),
        };
        let mut entries = Vec::new();
        for line in text.lines() {
            let value = line.trim();
            if !value.is_empty() {
                entries.push(value.to_string());
            }
        }
        entries
    }

    fn load_thumbnail(&mut self, key: &str) -> Option<ImageData> {
        let name = Self::thumbnail_name(key);
        let primary = format!("{}/{}", Self::thumbnails_dirname(), name);
        let legacy = format!("{}/{}", Self::thumbnails_dirname_legacy(), name);
        let mut file = self
            .fs
            .open_file(&primary, Mode::Read)
            .or_else(|_| self.fs.open_file(&legacy, Mode::Read))
            .ok()?;
        let mut header = [0u8; 16];
        let read = file.read(&mut header).ok()?;
        if read != header.len() || &header[0..4] != b"TRIM" {
            return None;
        }
        let width = u16::from_le_bytes([header[6], header[7]]) as u32;
        let height = u16::from_le_bytes([header[8], header[9]]) as u32;
        let plane = ((width as usize * height as usize) + 7) / 8;
        let expected = if header[4] == 2 && header[5] == 2 {
            plane * 3
        } else if header[4] == 1 && header[5] == 1 {
            plane
        } else {
            return None;
        };
        let mut bits = Vec::new();
        if bits.try_reserve(expected).is_err() {
            return None;
        }
        let mut buffer = [0u8; 256];
        while bits.len() < expected {
            let read = file.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            let remaining = expected - bits.len();
            let take = read.min(remaining);
            if bits.try_reserve(take).is_err() {
                return None;
            }
            bits.extend_from_slice(&buffer[..take]);
        }
        if bits.len() != expected {
            return None;
        }
        if expected == plane {
            Some(ImageData::Mono1 {
                width,
                height,
                bits,
            })
        } else {
            Some(ImageData::Gray2 {
                width,
                height,
                data: bits,
            })
        }
    }

    fn save_thumbnail(&mut self, key: &str, image: &ImageData) {
        let Some(data) = serialize_thumbnail(image) else {
            return;
        };
        let cache_name = Self::thumbnails_dirname();
        if self.fs.create_dir_all(cache_name).is_err() {
            return;
        }
        let name = Self::thumbnail_name(key);
        let path = format!("{}/{}", cache_name, name);
        let mut file = match self.fs.open_file(&path, Mode::Write) {
            Ok(file) => file,
            Err(_) => return,
        };
        if write_all(&mut file, &data).is_err() {
            return;
        }
        let _ = file.flush();
    }

    fn load_thumbnail_title(&mut self, key: &str) -> Option<String> {
        let name = Self::thumbnail_title_name(key);
        let primary = format!("{}/{}", Self::thumbnails_dirname(), name);
        let legacy = format!("{}/{}", Self::thumbnails_dirname_legacy(), name);
        let mut file = self
            .fs
            .open_file(&primary, Mode::Read)
            .or_else(|_| self.fs.open_file(&legacy, Mode::Read))
            .ok()?;
        let mut buf = [0u8; 128];
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            return None;
        }
        let text = core::str::from_utf8(&buf[..read]).ok()?.trim();
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    }

    fn save_thumbnail_title(&mut self, key: &str, title: &str) {
        let cache_name = Self::thumbnails_dirname();
        if self.fs.create_dir_all(cache_name).is_err() {
            return;
        }
        let name = Self::thumbnail_title_name(key);
        let path = format!("{}/{}", cache_name, name);
        let mut file = match self.fs.open_file(&path, Mode::Write) {
            Ok(file) => file,
            Err(_) => return,
        };
        if write_all(&mut file, title.as_bytes()).is_err() {
            return;
        }
        let _ = file.flush();
    }

}

impl<F> Gray2StreamSource for SdImageSource<F>
where
    F: Filesystem,
{
    fn load_gray2_stream(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
        rotation: tern_core::framebuffer::Rotation,
        base: &mut [u8],
        lsb: &mut [u8],
        msb: &mut [u8],
    ) -> Result<(), ImageError> {
        self.load_gray2_stream_region(key, width, height, rotation, base, lsb, msb, 0, 0)
    }

    fn load_gray2_stream_region(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
        rotation: tern_core::framebuffer::Rotation,
        base: &mut [u8],
        lsb: &mut [u8],
        msb: &mut [u8],
        dst_x: i32,
        dst_y: i32,
    ) -> Result<(), ImageError> {
        use tern_core::framebuffer::{HEIGHT as FB_HEIGHT, WIDTH as FB_WIDTH};

        fn map_point(
            rotation: tern_core::framebuffer::Rotation,
            x: usize,
            y: usize,
        ) -> Option<(usize, usize)> {
            let (x, y) = match rotation {
                tern_core::framebuffer::Rotation::Rotate0 => (x, y),
                tern_core::framebuffer::Rotation::Rotate90 => (y, FB_HEIGHT - 1 - x),
                tern_core::framebuffer::Rotation::Rotate180 => {
                    (FB_WIDTH - 1 - x, FB_HEIGHT - 1 - y)
                }
                tern_core::framebuffer::Rotation::Rotate270 => (FB_WIDTH - 1 - y, x),
            };
            if x >= FB_WIDTH || y >= FB_HEIGHT {
                None
            } else {
                Some((x, y))
            }
        }

        fn set_bit(buf: &mut [u8], x: usize, y: usize) {
            let idx = y * FB_WIDTH + x;
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            buf[byte] |= 1 << bit;
        }

        fn clear_bit(buf: &mut [u8], x: usize, y: usize) {
            let idx = y * FB_WIDTH + x;
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            buf[byte] &= !(1 << bit);
        }

        let mut load_from_reader = |reader: &mut dyn Read<Error = <F::File<'_> as embedded_io::ErrorType>::Error>|
            -> Result<(), ImageError> {
            let mut header = [0u8; 16];
            read_exact(reader, &mut header)?;
            if &header[0..4] != b"TRIM" || header[4] != 2 || header[5] != 2 {
                return Err(ImageError::Unsupported);
            }
            let w = u16::from_le_bytes([header[6], header[7]]) as u32;
            let h = u16::from_le_bytes([header[8], header[9]]) as u32;
            if w != width || h != height {
                return Err(ImageError::Decode);
            }

            let total_pixels = (width as usize) * (height as usize);
            let plane_len = (total_pixels + 7) / 8;
            let mut tmp = [0u8; 256];
            let mut pixel_index: usize = 0;
            let mut read_plane = |target: &mut [u8], is_base: bool| -> Result<(), ImageError> {
                pixel_index = 0;
                let mut remaining = plane_len;
                while remaining > 0 {
                    let want = remaining.min(tmp.len());
                    read_exact(reader, &mut tmp[..want])?;
                    for byte in &tmp[..want] {
                        for bit in 0..8 {
                            if pixel_index >= total_pixels {
                                break;
                            }
                            let sx = pixel_index % (width as usize);
                            let sy = pixel_index / (width as usize);
                            let bit_set = (byte >> (7 - bit)) & 0x01 == 1;
                            let dx = dst_x + sx as i32;
                            let dy = dst_y + sy as i32;
                            if dx >= 0 && dy >= 0 {
                                if let Some((fx, fy)) =
                                    map_point(rotation, dx as usize, dy as usize)
                                {
                                    if is_base {
                                        if bit_set {
                                            set_bit(target, fx, fy);
                                        } else {
                                            clear_bit(target, fx, fy);
                                        }
                                    } else if bit_set {
                                        set_bit(target, fx, fy);
                                    }
                                }
                            }
                            pixel_index += 1;
                        }
                    }
                    remaining -= want;
                }
                Ok(())
            };

            read_plane(base, true)?;
            read_plane(lsb, false)?;
            read_plane(msb, false)?;
            Ok(())
        };

        if let Some(offset_str) = key.strip_prefix("trbk:") {
            let offset: u32 = offset_str.parse().map_err(|_| ImageError::Decode)?;
            let Some(state) = &self.trbk else {
                return Err(ImageError::Decode);
            };
            let file_path = if state.path.is_empty() {
                state
                    .short_name
                    .as_deref()
                    .unwrap_or(state.name.as_str())
                    .to_string()
            } else {
                format!("{}/{}", state.path.join("/"), state.name)
            };
            let mut file = self
                .fs
                .open_file(&file_path, Mode::Read)
                .map_err(|_| ImageError::Io)?;
            file.seek(SeekFrom::Start(offset as u64))
                .map_err(|_| ImageError::Io)?;
            return load_from_reader(&mut file);
        }

        let mut file = self
            .fs
            .open_file(key, Mode::Read)
            .map_err(|_| ImageError::Io)?;
        load_from_reader(&mut file)
    }

    fn load_gray2_stream_thumbnail(
        &mut self,
        key: &str,
        width: u32,
        height: u32,
        thumb_w: u32,
        thumb_h: u32,
    ) -> Option<ImageData> {
        fn set_bit(buf: &mut [u8], x: usize, y: usize, width: usize, value: bool) {
            let idx = y * width + x;
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            if value {
                buf[byte] |= 1 << bit;
            } else {
                buf[byte] &= !(1 << bit);
            }
        }

        fn set_bit_on(buf: &mut [u8], x: usize, y: usize, width: usize) {
            let idx = y * width + x;
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            buf[byte] |= 1 << bit;
        }

        fn alloc_u16(len: usize) -> Option<Vec<u16>> {
            let mut out = Vec::new();
            if out.try_reserve_exact(len).is_err() {
                return None;
            }
            out.resize(len, 0);
            Some(out)
        }

        fn alloc_u8(len: usize, fill: u8) -> Option<Vec<u8>> {
            let mut out = Vec::new();
            if out.try_reserve_exact(len).is_err() {
                return None;
            }
            out.resize(len, fill);
            Some(out)
        }

        let total_pixels = (width as usize) * (height as usize);
        if total_pixels == 0 {
            return None;
        }
        let thumb_w = thumb_w.max(1) as usize;
        let thumb_h = thumb_h.max(1) as usize;
        let thumb_pixels = thumb_w * thumb_h;
        let thumb_plane = (thumb_pixels + 7) / 8;
        let mut sum_bw = alloc_u16(thumb_pixels)?;
        let mut sum_l = alloc_u16(thumb_pixels)?;
        let mut sum_m = alloc_u16(thumb_pixels)?;
        let mut counts = alloc_u16(thumb_pixels)?;

        let mut load_from_reader = |reader: &mut dyn Read<Error = <F::File<'_> as embedded_io::ErrorType>::Error>|
            -> Result<(), ImageError> {
            let mut header = [0u8; 16];
            read_exact(reader, &mut header)?;
            if &header[0..4] != b"TRIM" || header[4] != 2 || header[5] != 2 {
                return Err(ImageError::Unsupported);
            }
            let w = u16::from_le_bytes([header[6], header[7]]) as u32;
            let h = u16::from_le_bytes([header[8], header[9]]) as u32;
            if w != width || h != height {
                return Err(ImageError::Decode);
            }

            let plane_len = (total_pixels + 7) / 8;
            let mut tmp = [0u8; 256];
            let mut pixel_index = 0usize;
            let mut read_plane = |sum: &mut [u16], track_count: bool| -> Result<(), ImageError> {
                pixel_index = 0;
                let mut remaining = plane_len;
                while remaining > 0 {
                    let want = remaining.min(tmp.len());
                    read_exact(reader, &mut tmp[..want])?;
                    for byte in &tmp[..want] {
                        for bit in 0..8 {
                            if pixel_index >= total_pixels {
                                break;
                            }
                            let sx = pixel_index % (width as usize);
                            let sy = pixel_index / (width as usize);
                            let dx = (sx * thumb_w) / (width as usize);
                            let dy = (sy * thumb_h) / (height as usize);
                            let bit_set = (byte >> (7 - bit)) & 0x01;
                            if dx < thumb_w && dy < thumb_h {
                                let dst = dy * thumb_w + dx;
                                if track_count {
                                    counts[dst] = counts[dst].saturating_add(1);
                                }
                                sum[dst] = sum[dst].saturating_add(bit_set as u16);
                            }
                            pixel_index += 1;
                        }
                    }
                    remaining -= want;
                }
                Ok(())
            };

            read_plane(&mut sum_bw, true)?;
            read_plane(&mut sum_l, false)?;
            read_plane(&mut sum_m, false)?;
            Ok(())
        };

        let result = if let Some(offset_str) = key.strip_prefix("trbk:") {
            let offset: u32 = offset_str.parse().ok()?;
            let state = self.trbk.as_ref()?;
            let file_path = if state.path.is_empty() {
                state
                    .short_name
                    .as_deref()
                    .unwrap_or(state.name.as_str())
                    .to_string()
            } else {
                format!("{}/{}", state.path.join("/"), state.name)
            };
            let mut file = self.fs.open_file(&file_path, Mode::Read).ok()?;
            file.seek(SeekFrom::Start(offset as u64)).ok()?;
            load_from_reader(&mut file)
        } else {
            let mut file = self.fs.open_file(key, Mode::Read).ok()?;
            load_from_reader(&mut file)
        };

        if result.is_err() {
            return None;
        }

        let mut bits = alloc_u8(thumb_plane, 0xFF)?;
        for idx in 0..thumb_pixels {
            let count = counts[idx].max(1) as i32;
            let avg_bw = sum_bw[idx] as i32;
            let avg_l = sum_l[idx] as i32;
            let avg_m = sum_m[idx] as i32;
            let mut lum = (255 * avg_bw + 128 * avg_m - 64 * avg_l) / count;
            if lum < 0 {
                lum = 0;
            } else if lum > 255 {
                lum = 255;
            }
            let lum = adjust_thumbnail_luma(lum as u8);
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            if lum >= 128 {
                bits[byte] |= 1 << bit;
            } else {
                bits[byte] &= !(1 << bit);
            }
        }

        Some(ImageData::Mono1 {
            width: thumb_w as u32,
            height: thumb_h as u32,
            bits,
        })
    }
}

impl<F> BookSource for SdImageSource<F>
where
    F: Filesystem,
{
    fn load_trbk(
        &mut self,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<tern_core::trbk::TrbkBook, ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Unsupported);
        }
        let file_path = Self::build_path(path, &entry.name);
        let mut file = self
            .fs
            .open_file(&file_path, Mode::Read)
            .map_err(|_| ImageError::Io)?;
        let file_len = file.size();

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

        tern_core::trbk::parse_trbk(&data)
    }

    fn open_trbk(
        &mut self,
        path: &[String],
        entry: &ImageEntry,
    ) -> Result<Rc<tern_core::trbk::TrbkBookInfo>, ImageError> {
        if entry.kind != EntryKind::File {
            return Err(ImageError::Unsupported);
        }
        let file_path = Self::build_path(path, &entry.name);
        let mut file = self
            .fs
            .open_file(&file_path, Mode::Read)
            .map_err(|_| ImageError::Io)?;

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
        let margin_bottom = read_u16_le(&header_buf, cursor)?;

        let metadata = tern_core::trbk::TrbkMetadata {
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
                toc_entries.push(tern_core::trbk::TrbkTocEntry {
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
                let plane_len = ((width as usize * height as usize) + 7) / 8;
                let (bitmap_bw, bitmap_lsb, bitmap_msb) = if bitmap_len == plane_len * 3 {
                    let bw = bitmap[0..plane_len].to_vec();
                    let lsb = bitmap[plane_len..plane_len * 2].to_vec();
                    let msb = bitmap[plane_len * 2..plane_len * 3].to_vec();
                    (bw, Some(lsb), Some(msb))
                } else {
                    (bitmap, None, None)
                };
                glyphs.push(tern_core::trbk::TrbkGlyph {
                    codepoint,
                    style,
                    width,
                    height,
                    x_advance,
                    x_offset,
                    y_offset,
                    bitmap_bw,
                    bitmap_lsb,
                    bitmap_msb,
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

            let parse_entry = |entry_buf: &[u8]| {
                let rel_offset = u32::from_le_bytes([entry_buf[0], entry_buf[1], entry_buf[2], entry_buf[3]]);
                let data_len = u32::from_le_bytes([entry_buf[4], entry_buf[5], entry_buf[6], entry_buf[7]]);
                let width = u16::from_le_bytes([entry_buf[8], entry_buf[9]]);
                let height = u16::from_le_bytes([entry_buf[10], entry_buf[11]]);
                (rel_offset, data_len, width, height)
            };

            if image_count > 0 {
                let (rel_offset, data_len, width, height) = parse_entry(&first_buf);
                let data_offset = images_offset.saturating_add(rel_offset);
                images.push(tern_core::trbk::TrbkImageInfo {
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
                    images.push(tern_core::trbk::TrbkImageInfo {
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
                    images.push(tern_core::trbk::TrbkImageInfo {
                        data_offset,
                        data_len,
                        width,
                        height,
                    });
                }
            }
        }

        let glyphs = Rc::new(glyphs);
        let info = Rc::new(tern_core::trbk::TrbkBookInfo {
            screen_width,
            screen_height,
            page_count,
            metadata,
            glyphs: glyphs.clone(),
            toc: toc_entries,
            images,
        });

        self.trbk = Some(TrbkStream {
            path: path.to_vec(),
            name: entry.name.clone(),
            short_name: self.lookup_short_name(&entry.name),
            page_offsets: offsets,
            page_data_offset,
            glyph_table_offset,
            info: info.clone(),
        });

        Ok(info)
    }

    fn trbk_page(&mut self, page_index: usize) -> Result<tern_core::trbk::TrbkPage, ImageError> {
        let Some(state) = &self.trbk else {
            return Err(ImageError::Decode);
        };
        if page_index >= state.page_offsets.len() {
            return Err(ImageError::Decode);
        }
        let file_path = if state.path.is_empty() {
            state
                .short_name
                .as_deref()
                .unwrap_or(state.name.as_str())
                .to_string()
        } else {
            Self::build_path(&state.path, &state.name)
        };
        let mut file = self
            .fs
            .open_file(&file_path, Mode::Read)
            .map_err(|_| ImageError::Io)?;

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
        let ops = tern_core::trbk::parse_trbk_page_ops(&buf)?;
        Ok(tern_core::trbk::TrbkPage { ops })
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
        let file_path = if state.path.is_empty() {
            state
                .short_name
                .as_deref()
                .unwrap_or(state.name.as_str())
                .to_string()
        } else {
            Self::build_path(&state.path, &state.name)
        };
        let mut file = self
            .fs
            .open_file(&file_path, Mode::Read)
            .map_err(|_| ImageError::Io)?;
        file.seek(SeekFrom::Start(image.data_offset as u64))
            .map_err(|_| ImageError::Io)?;
        let mut header = [0u8; 16];
        read_exact(&mut file, &mut header)?;
        if &header[0..4] == b"TRIM" && header[4] == 2 && header[5] == 2 {
            let w = u16::from_le_bytes([header[6], header[7]]) as u32;
            let h = u16::from_le_bytes([header[8], header[9]]) as u32;
            if w == image.width as u32 && h == image.height as u32 {
                let plane_len = ((w as usize * h as usize) + 7) / 8;
                if plane_len.saturating_mul(3) >= tern_core::framebuffer::BUFFER_SIZE {
                    // For large grayscale images, stream directly from TRBK to avoid heap.
                    let key = alloc::format!("trbk:{}", image.data_offset);
                    return Ok(ImageData::Gray2Stream { width: w, height: h, key });
                }
            }
        }
        file.seek(SeekFrom::Start(image.data_offset as u64))
            .map_err(|_| ImageError::Io)?;
        read_trimg_from_file(&mut file, image.data_len as usize)
    }

    fn close_trbk(&mut self) {
        self.trbk = None;
    }
}

impl<F> PowerSource for SdImageSource<F>
where
    F: Filesystem,
{
}


fn adjust_thumbnail_luma(lum: u8) -> u8 {
    let mut value = ((lum as i32 - 128) * 13) / 10 + 128;
    if value < 0 {
        value = 0;
    } else if value > 255 {
        value = 255;
    }
    value as u8
}
