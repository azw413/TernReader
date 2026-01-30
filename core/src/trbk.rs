extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::image_viewer::ImageError;

#[derive(Clone, Debug)]
pub struct TrbkMetadata {
    pub title: String,
    pub author: String,
    pub language: String,
    pub identifier: String,
    pub font_name: String,
    pub char_width: u16,
    pub line_height: u16,
    pub ascent: i16,
    pub margin_left: u16,
    pub margin_right: u16,
    pub margin_top: u16,
    pub margin_bottom: u16,
}

#[derive(Clone, Debug)]
pub struct TrbkBook {
    pub screen_width: u16,
    pub screen_height: u16,
    pub pages: Vec<TrbkPage>,
    pub metadata: TrbkMetadata,
    pub glyphs: Vec<TrbkGlyph>,
    pub page_count: usize,
    pub toc: Vec<TrbkTocEntry>,
    pub images: Vec<TrbkImageInfo>,
}

#[derive(Clone, Debug)]
pub struct TrbkBookInfo {
    pub screen_width: u16,
    pub screen_height: u16,
    pub page_count: usize,
    pub metadata: TrbkMetadata,
    pub glyphs: Vec<TrbkGlyph>,
    pub toc: Vec<TrbkTocEntry>,
    pub images: Vec<TrbkImageInfo>,
}

#[derive(Clone, Debug)]
pub struct TrbkPage {
    pub ops: Vec<TrbkOp>,
}

#[derive(Clone, Debug)]
pub enum TrbkOp {
    TextRun { x: i32, y: i32, style: u8, text: String },
    Image {
        x: i32,
        y: i32,
        width: u16,
        height: u16,
        image_index: u16,
    },
}

#[derive(Clone, Debug)]
pub struct TrbkGlyph {
    pub codepoint: u32,
    pub style: u8,
    pub width: u8,
    pub height: u8,
    pub x_advance: i16,
    pub x_offset: i16,
    pub y_offset: i16,
    pub bitmap: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct TrbkTocEntry {
    pub title: String,
    pub page_index: u32,
    pub level: u8,
}

#[derive(Clone, Debug)]
pub struct TrbkImageInfo {
    pub data_offset: u32,
    pub data_len: u32,
    pub width: u16,
    pub height: u16,
}

pub fn parse_trbk(data: &[u8]) -> Result<TrbkBook, ImageError> {
    if data.len() < 0x2C || &data[0..4] != b"TRBK" {
        return Err(ImageError::Decode);
    }

    let version = data[4];
    if version != 1 && version != 2 {
        return Err(ImageError::Unsupported);
    }

    let header_size = read_u16(data, 0x06)? as usize;
    let screen_width = read_u16(data, 0x08)?;
    let screen_height = read_u16(data, 0x0A)?;
    let page_count = read_u32(data, 0x0C)? as usize;
    let toc_count = read_u32(data, 0x10)? as usize;
    let page_lut_offset = read_u32(data, 0x14)? as usize;
    let toc_offset = read_u32(data, 0x18)? as usize;
    let page_data_offset = read_u32(data, 0x1C)? as usize;
    let images_offset = if version >= 2 {
        read_u32(data, 0x20)? as usize
    } else {
        0
    };
    let (glyph_count, glyph_table_offset) = if version >= 2 {
        (read_u32(data, 0x28)? as usize, read_u32(data, 0x2C)? as usize)
    } else {
        (0usize, 0usize)
    };

    if data.len() < header_size || toc_offset != header_size {
        return Err(ImageError::Decode);
    }
    if toc_count != 0 && data.len() < toc_offset {
        return Err(ImageError::Decode);
    }
    if data.len() < page_lut_offset {
        return Err(ImageError::Decode);
    }

    let mut cursor = if version >= 2 { 0x30 } else { 0x2C };
    let title = read_string(data, &mut cursor)?;
    let author = read_string(data, &mut cursor)?;
    let language = read_string(data, &mut cursor)?;
    let identifier = read_string(data, &mut cursor)?;
    let font_name = read_string(data, &mut cursor)?;
    let char_width = read_u16_from(data, &mut cursor)?;
    let line_height = read_u16_from(data, &mut cursor)?;
    let remaining = header_size.saturating_sub(cursor);
    let (ascent, margin_left, margin_right, margin_top, margin_bottom) = if remaining >= 12 {
        let ascent = read_i16_from(data, &mut cursor)?;
        let margin_left = read_u16_from(data, &mut cursor)?;
        let margin_right = read_u16_from(data, &mut cursor)?;
        let margin_top = read_u16_from(data, &mut cursor)?;
        let margin_bottom = read_u16_from(data, &mut cursor)?;
        (ascent, margin_left, margin_right, margin_top, margin_bottom)
    } else {
        let margin_left = read_u16_from(data, &mut cursor)?;
        let margin_right = read_u16_from(data, &mut cursor)?;
        let margin_top = read_u16_from(data, &mut cursor)?;
        let margin_bottom = read_u16_from(data, &mut cursor)?;
        let ascent = (line_height as i16).saturating_sub((line_height as i16) / 4);
        (ascent, margin_left, margin_right, margin_top, margin_bottom)
    };

    if cursor > data.len() || cursor > header_size {
        return Err(ImageError::Decode);
    }

    let toc = if toc_count > 0 {
        parse_trbk_toc(data, toc_offset as usize, toc_count)?
    } else {
        Vec::new()
    };

    let images = if images_offset > 0 {
        parse_trbk_images(data, images_offset)?
    } else {
        Vec::new()
    };

    let lut_len = page_count * 4;
    if page_lut_offset + lut_len > data.len() {
        return Err(ImageError::Decode);
    }

    let mut page_offsets = Vec::with_capacity(page_count);
    for i in 0..page_count {
        let pos = page_lut_offset + i * 4;
        page_offsets.push(read_u32(data, pos)? as usize);
    }

    let mut pages = Vec::with_capacity(page_count);
    for (idx, offset) in page_offsets.iter().enumerate() {
        let start = page_data_offset + offset;
        let end = if idx + 1 < page_offsets.len() {
            page_data_offset + page_offsets[idx + 1]
        } else if version >= 2 && glyph_table_offset > page_data_offset {
            glyph_table_offset
        } else {
            data.len()
        };
        if start > data.len() || end > data.len() || start > end {
            return Err(ImageError::Decode);
        }
        let ops = parse_trbk_page_ops(&data[start..end])?;
        pages.push(TrbkPage { ops });
    }

    let glyphs = if version >= 2 && glyph_count > 0 {
        parse_glyphs(data, glyph_table_offset, glyph_count)?
    } else {
        Vec::new()
    };

    Ok(TrbkBook {
        screen_width,
        screen_height,
        pages,
        metadata: TrbkMetadata {
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
        },
        glyphs,
        page_count,
        toc,
        images,
    })
}

impl TrbkBook {
    pub fn info(&self) -> TrbkBookInfo {
        TrbkBookInfo {
            screen_width: self.screen_width,
            screen_height: self.screen_height,
            page_count: self.page_count,
            metadata: self.metadata.clone(),
            glyphs: self.glyphs.clone(),
            toc: self.toc.clone(),
            images: self.images.clone(),
        }
    }
}

fn parse_trbk_toc(
    data: &[u8],
    offset: usize,
    count: usize,
) -> Result<Vec<TrbkTocEntry>, ImageError> {
    if offset > data.len() {
        return Err(ImageError::Decode);
    }
    let mut cursor = offset;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let title = read_string(data, &mut cursor)?;
        if cursor + 4 + 1 + 1 + 2 > data.len() {
            return Err(ImageError::Decode);
        }
        let page_index = read_u32(data, cursor)?;
        cursor += 4;
        let level = data[cursor];
        cursor += 1;
        cursor += 1; // reserved
        cursor += 2; // reserved
        entries.push(TrbkTocEntry {
            title,
            page_index,
            level,
        });
    }
    Ok(entries)
}

pub fn parse_trbk_page_ops(data: &[u8]) -> Result<Vec<TrbkOp>, ImageError> {
    let mut ops = Vec::new();
    let mut cursor = 0usize;
    while cursor + 3 <= data.len() {
        let opcode = data[cursor];
        let length = u16::from_le_bytes([data[cursor + 1], data[cursor + 2]]) as usize;
        cursor += 3;
        if cursor + length > data.len() {
            return Err(ImageError::Decode);
        }
        let payload = &data[cursor..cursor + length];
        cursor += length;

        match opcode {
            0x01 => {
                if payload.len() < 6 {
                    return Err(ImageError::Decode);
                }
                let x = u16::from_le_bytes([payload[0], payload[1]]) as i32;
                let y = u16::from_le_bytes([payload[2], payload[3]]) as i32;
                let style = payload[4];
                let text = core::str::from_utf8(&payload[6..])
                    .map_err(|_| ImageError::Decode)?
                    .to_string();
                ops.push(TrbkOp::TextRun { x, y, style, text });
            }
            0x02 => {
                if payload.len() < 12 {
                    return Err(ImageError::Decode);
                }
                let x = u16::from_le_bytes([payload[0], payload[1]]) as i32;
                let y = u16::from_le_bytes([payload[2], payload[3]]) as i32;
                let width = u16::from_le_bytes([payload[4], payload[5]]);
                let height = u16::from_le_bytes([payload[6], payload[7]]);
                let image_index = u16::from_le_bytes([payload[8], payload[9]]);
                ops.push(TrbkOp::Image {
                    x,
                    y,
                    width,
                    height,
                    image_index,
                });
            }
            _ => {
                // Ignore unknown ops for forward compatibility.
            }
        }
    }
    Ok(ops)
}

fn parse_trbk_images(data: &[u8], offset: usize) -> Result<Vec<TrbkImageInfo>, ImageError> {
    if offset + 4 > data.len() {
        return Err(ImageError::Decode);
    }
    let count = read_u32(data, offset)? as usize;
    let mut cursor = offset + 4;
    let remaining = data.len().saturating_sub(cursor);
    let entry_size = if remaining >= count * 16 {
        16
    } else if remaining >= count * 14 {
        14
    } else {
        return Err(ImageError::Decode);
    };
    let mut images = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + entry_size > data.len() {
            return Err(ImageError::Decode);
        }
        let rel_offset = read_u32(data, cursor)?;
        cursor += 4;
        let data_len = read_u32(data, cursor)?;
        cursor += 4;
        let width = read_u16(data, cursor)?;
        cursor += 2;
        let height = read_u16(data, cursor)?;
        cursor += 2;
        cursor += 2; // reserved
        if entry_size == 16 {
            cursor += 2; // reserved padding
        }
        let data_offset = offset as u32 + rel_offset;
        if data_offset as usize + data_len as usize > data.len() {
            return Err(ImageError::Decode);
        }
        images.push(TrbkImageInfo {
            data_offset,
            data_len,
            width,
            height,
        });
    }
    Ok(images)
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, ImageError> {
    if offset + 2 > data.len() {
        return Err(ImageError::Decode);
    }
    Ok(u16::from_le_bytes([data[offset], data[offset + 1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, ImageError> {
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

fn read_u16_from(data: &[u8], cursor: &mut usize) -> Result<u16, ImageError> {
    let value = read_u16(data, *cursor)?;
    *cursor += 2;
    Ok(value)
}

fn read_i16_from(data: &[u8], cursor: &mut usize) -> Result<i16, ImageError> {
    if *cursor + 2 > data.len() {
        return Err(ImageError::Decode);
    }
    let value = i16::from_le_bytes([data[*cursor], data[*cursor + 1]]);
    *cursor += 2;
    Ok(value)
}

fn read_string(data: &[u8], cursor: &mut usize) -> Result<String, ImageError> {
    let len = read_u32(data, *cursor)? as usize;
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

fn parse_glyphs(
    data: &[u8],
    offset: usize,
    count: usize,
) -> Result<Vec<TrbkGlyph>, ImageError> {
    if offset > data.len() {
        return Err(ImageError::Decode);
    }
    let mut cursor = offset;
    let mut glyphs = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 4 + 1 + 1 + 1 + 2 + 2 + 2 + 4 > data.len() {
            return Err(ImageError::Decode);
        }
        let codepoint = read_u32(data, cursor)?;
        cursor += 4;
        let style = data[cursor];
        cursor += 1;
        let width = data[cursor];
        cursor += 1;
        let height = data[cursor];
        cursor += 1;
        let x_advance = i16::from_le_bytes([data[cursor], data[cursor + 1]]);
        cursor += 2;
        let x_offset = i16::from_le_bytes([data[cursor], data[cursor + 1]]);
        cursor += 2;
        let y_offset = i16::from_le_bytes([data[cursor], data[cursor + 1]]);
        cursor += 2;
        let bitmap_len = read_u32(data, cursor)? as usize;
        cursor += 4;
        if cursor + bitmap_len > data.len() {
            return Err(ImageError::Decode);
        }
        let bitmap = data[cursor..cursor + bitmap_len].to_vec();
        cursor += bitmap_len;
        glyphs.push(TrbkGlyph {
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
    Ok(glyphs)
}
