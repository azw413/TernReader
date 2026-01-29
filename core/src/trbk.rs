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
}

#[derive(Clone, Debug)]
pub struct TrbkPage {
    pub ops: Vec<TrbkOp>,
}

#[derive(Clone, Debug)]
pub enum TrbkOp {
    TextRun { x: i32, y: i32, style: u8, text: String },
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
    let _images_offset = read_u32(data, 0x20)? as usize;
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

    if toc_count != 0 {
        // Skip TOC for now.
    }

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
        let ops = parse_page_ops(&data[start..end])?;
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
    })
}

fn parse_page_ops(data: &[u8]) -> Result<Vec<TrbkOp>, ImageError> {
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
            _ => {
                // Ignore unknown ops for forward compatibility.
            }
        }
    }
    Ok(ops)
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
