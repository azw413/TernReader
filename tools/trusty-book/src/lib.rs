use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BookError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("epub error: {0}")]
    Epub(#[from] trusty_epub::EpubError),
    #[error("invalid output")]
    InvalidOutput,
}

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub screen_width: u16,
    pub screen_height: u16,
    pub margin_x: u16,
    pub margin_y: u16,
    pub line_height: u16,
    pub char_width: u16,
    pub ascent: i16,
    pub word_spacing: i16,
    pub max_spine_items: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            screen_width: 480,
            screen_height: 800,
            margin_x: 16,
            margin_y: 60,
            line_height: 20,
            char_width: 10,
            ascent: 14,
            word_spacing: 2,
            max_spine_items: 50,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TrbkMetadata {
    pub title: String,
    pub author: String,
    pub language: String,
    pub identifier: String,
}

#[derive(Clone, Debug, Default)]
pub struct FontPaths {
    pub regular: Option<String>,
    pub bold: Option<String>,
    pub italic: Option<String>,
    pub bold_italic: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum StyleId {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub codepoint: u32,
    pub style: StyleId,
    pub width: u8,
    pub height: u8,
    pub x_advance: i16,
    pub x_offset: i16,
    pub y_offset: i16,
    pub bitmap: Vec<u8>,
}

#[derive(Clone, Debug)]
struct RunLine {
    runs: Vec<trusty_epub::TextRun>,
}

pub fn convert_epub_to_trbk<P: AsRef<Path>, Q: AsRef<Path>>(
    epub_path: P,
    output_path: Q,
    options: &RenderOptions,
) -> Result<(), BookError> {
    convert_epub_to_trbk_multi(epub_path, output_path, &[options.char_width], &FontPaths::default())
}

pub fn convert_epub_to_trbk_multi<P: AsRef<Path>, Q: AsRef<Path>>(
    epub_path: P,
    output_path: Q,
    sizes: &[u16],
    font_paths: &FontPaths,
) -> Result<(), BookError> {
    let epub_path = epub_path.as_ref();
    let output_path = output_path.as_ref();
    let cache_dir = trusty_epub::default_cache_dir(epub_path);
    let (cache, _) = trusty_epub::load_or_build_cache(epub_path, &cache_dir)?;

    let metadata = TrbkMetadata {
        title: cache
            .metadata
            .title
            .as_deref()
            .unwrap_or("<unknown>")
            .to_string(),
        author: cache
            .metadata
            .creator
            .as_deref()
            .unwrap_or("<unknown>")
            .to_string(),
        language: cache
            .metadata
            .language
            .as_deref()
            .unwrap_or("<unknown>")
            .to_string(),
        identifier: cache
            .metadata
            .identifier
            .as_deref()
            .unwrap_or("<unknown>")
            .to_string(),
    };

    let runs = extract_runs(epub_path, &cache, 200)?;
    let used = collect_used_codepoints(&runs);
    let font_set = load_fonts(font_paths)?;

    let sizes = if sizes.is_empty() { vec![10] } else { sizes.to_vec() };
    let multi = sizes.len() > 1;
    for size in &sizes {
        let mut options = RenderOptions::default();
        let regular = font_set
            .get(&StyleId::Regular)
            .ok_or(BookError::InvalidOutput)?;
        let (metrics, _) = regular.rasterize('n', *size as f32);
        options.char_width = metrics.advance_width.round().max(1.0) as u16;
        let mut codepoints = used
            .get(&StyleId::Regular)
            .cloned()
            .unwrap_or_default();
        if codepoints.is_empty() {
            for set in used.values() {
                codepoints.extend(set.iter().copied());
            }
        }
        let ascent = compute_ascent(regular, *size, &codepoints);
        options.ascent = ascent;
        if let Some(lines) = regular.horizontal_line_metrics(*size as f32) {
            let height = (lines.ascent - lines.descent + lines.line_gap)
                .ceil()
                .max(1.0) as u16;
            let extra = (height / 6).max(2);
            options.line_height = height.saturating_add(extra);
        } else {
            options.line_height = size.saturating_mul(2);
        }
        options.word_spacing = (options.char_width as i16 / 3).max(2);
        let output = output_path_for_size(output_path, *size, multi);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let glyphs = build_glyphs(&font_set, *size, &used)?;
        let advance_map = build_advance_map(&glyphs);
        let lines = wrap_runs(&runs, &options, &advance_map);
        let pages = paginate_lines(&lines, &options);
        write_trbk(&output, &metadata, &options, &pages, &glyphs, &advance_map)?;
    }

    Ok(())
}

fn extract_runs(
    epub_path: &Path,
    cache: &trusty_epub::BookCache,
    max_spine_items: usize,
) -> Result<Vec<trusty_epub::TextRun>, BookError> {
    let mut runs = Vec::new();
    let max_try = cache.spine.len().min(max_spine_items).max(1);
    for index in 0..max_try {
        let xhtml = match trusty_epub::read_spine_xhtml(epub_path, index) {
            Ok(xhtml) => xhtml,
            Err(_) => continue,
        };
        let blocks = match trusty_epub::parse_xhtml_blocks(&xhtml) {
            Ok(blocks) => blocks,
            Err(_) => continue,
        };
        let block_runs = trusty_epub::blocks_to_runs(&blocks);
        if !block_runs.is_empty() {
            runs.extend(block_runs);
        }
        if runs.len() > 50000 {
            break;
        }
    }
    Ok(runs)
}

fn wrap_runs(
    runs: &[trusty_epub::TextRun],
    options: &RenderOptions,
    advance_map: &HashMap<(StyleId, u32), i16>,
) -> Vec<RunLine> {
    let max_width = (options.screen_width as i32 - options.margin_x as i32 * 2).max(1);
    let mut lines = Vec::new();
    let mut current: Vec<trusty_epub::TextRun> = Vec::new();
    let mut current_width = 0i32;

    for run in runs {
        for token in run.text.split_whitespace() {
            let token_width = measure_token_width(token, run.style, options, advance_map);
            if current_width == 0 {
                current.push(trusty_epub::TextRun {
                    text: token.to_string(),
                    style: run.style,
                });
                current_width = token_width;
                continue;
            }
            let space_width =
                measure_token_width(" ", run.style, options, advance_map) + options.word_spacing as i32;
            if current_width + space_width + token_width <= max_width {
                current.push(trusty_epub::TextRun {
                    text: " ".to_string(),
                    style: run.style,
                });
                current.push(trusty_epub::TextRun {
                    text: token.to_string(),
                    style: run.style,
                });
                current_width += space_width + token_width;
                continue;
            }
            lines.push(RunLine { runs: current });
            current = Vec::new();
            current.push(trusty_epub::TextRun {
                text: token.to_string(),
                style: run.style,
            });
            current_width = token_width;
        }
        if run.text.contains('\n') {
            if !current.is_empty() {
                lines.push(RunLine { runs: current });
                current = Vec::new();
                current_width = 0;
            }
        }
    }
    if !current.is_empty() {
        lines.push(RunLine { runs: current });
    }
    lines
}

fn paginate_lines(lines: &[RunLine], options: &RenderOptions) -> Vec<RunLine> {
    let usable_height = options
        .screen_height
        .saturating_sub(options.margin_y * 2)
        .max(1);
    let lines_per_page = (usable_height as usize / options.line_height as usize).max(1);
    let mut pages = Vec::new();
    let mut idx = 0;
    while idx < lines.len() {
        let end = (idx + lines_per_page).min(lines.len());
        let mut page_runs = Vec::new();
        for line in &lines[idx..end] {
            page_runs.extend(line.runs.clone());
            page_runs.push(trusty_epub::TextRun {
                text: "\n".to_string(),
                style: trusty_epub::TextStyle::default(),
            });
        }
        pages.push(RunLine { runs: page_runs });
        idx = end;
    }
    if pages.is_empty() {
        pages.push(RunLine {
            runs: vec![trusty_epub::TextRun {
                text: "(empty)".to_string(),
                style: trusty_epub::TextStyle::default(),
            }],
        });
    }
    pages
}

fn build_advance_map(glyphs: &[Glyph]) -> HashMap<(StyleId, u32), i16> {
    let mut map = HashMap::new();
    for glyph in glyphs {
        map.insert((glyph.style, glyph.codepoint), glyph.x_advance);
    }
    map
}

fn compute_ascent(font: &fontdue::Font, size: u16, codepoints: &BTreeSet<u32>) -> i16 {
    let mut cap_ascent = 0i16;
    let mut ascent = 0i16;
    for cp in codepoints {
        if let Some(ch) = char::from_u32(*cp) {
            let (metrics, _) = font.rasterize(ch, size as f32);
            let candidate = (metrics.ymin + metrics.height as i32).max(0) as i16;
            if ch.is_ascii_uppercase() && candidate > cap_ascent {
                cap_ascent = candidate;
            }
            if candidate > ascent {
                ascent = candidate;
            }
        }
    }
    let picked = if cap_ascent > 0 { cap_ascent } else { ascent };
    if picked == 0 {
        size as i16
    } else {
        picked
    }
}

fn measure_token_width(
    text: &str,
    style: trusty_epub::TextStyle,
    options: &RenderOptions,
    advance_map: &HashMap<(StyleId, u32), i16>,
) -> i32 {
    let mut width = 0i32;
    let style_id = style_id_from_style(style);
    for ch in text.chars() {
        let cp = ch as u32;
        if let Some(adv) = advance_map.get(&(style_id, cp)) {
            width += *adv as i32;
        } else {
            width += options.char_width as i32;
        }
    }
    width
}

fn write_trbk(
    path: &Path,
    metadata: &TrbkMetadata,
    options: &RenderOptions,
    pages: &[RunLine],
    glyphs: &[Glyph],
    advance_map: &HashMap<(StyleId, u32), i16>,
) -> Result<(), BookError> {
    let mut file = File::create(path)?;

    let toc_count: u32 = 0;
    let page_count = pages.len() as u32;
    let glyph_count = glyphs.len() as u32;

    let fixed_header_size: u16 = 0x30;

    let mut metadata_bytes = Vec::new();
    write_string(&mut metadata_bytes, &metadata.title)?;
    write_string(&mut metadata_bytes, &metadata.author)?;
    write_string(&mut metadata_bytes, &metadata.language)?;
    write_string(&mut metadata_bytes, &metadata.identifier)?;
    write_string(&mut metadata_bytes, "fontdue")?;
    metadata_bytes.extend_from_slice(&options.char_width.to_le_bytes());
    metadata_bytes.extend_from_slice(&options.line_height.to_le_bytes());
    metadata_bytes.extend_from_slice(&options.ascent.to_le_bytes());
    metadata_bytes.extend_from_slice(&options.margin_x.to_le_bytes());
    metadata_bytes.extend_from_slice(&options.margin_x.to_le_bytes());
    metadata_bytes.extend_from_slice(&options.margin_y.to_le_bytes());
    metadata_bytes.extend_from_slice(&options.margin_y.to_le_bytes());

    let header_size: u16 = fixed_header_size + metadata_bytes.len() as u16;
    let toc_offset: u32 = header_size as u32;
    let page_lut_offset: u32 = toc_offset + toc_count * 12;

    let mut page_lut = Vec::new();
    let mut page_data = Vec::new();

    for page in pages {
        let page_start = page_data.len() as u32;
        page_lut.extend_from_slice(&page_start.to_le_bytes());

        let mut baseline = options.margin_y as i32 + options.ascent as i32;
        let mut x = options.margin_x as u16;
        for run in &page.runs {
            if run.text == "\n" {
                baseline += options.line_height as i32;
                x = options.margin_x;
                continue;
            }
            let mut payload = Vec::new();
            payload.extend_from_slice(&x.to_le_bytes());
            payload.extend_from_slice(&(baseline as u16).to_le_bytes());
            payload.push(style_id_from_style(run.style) as u8);
            payload.push(0);
            payload.extend_from_slice(run.text.as_bytes());
            let length = payload.len() as u16;
            page_data.push(0x01);
            page_data.extend_from_slice(&length.to_le_bytes());
            page_data.extend_from_slice(&payload);
            let mut advance = 0i32;
            let style_id = style_id_from_style(run.style);
            for ch in run.text.chars() {
                let cp = ch as u32;
                if let Some(x_adv) = advance_map.get(&(style_id, cp)) {
                    advance += *x_adv as i32;
                } else {
                    advance += options.char_width as i32;
                }
            }
            if advance > 0 {
                x = x.saturating_add(advance as u16);
            }
        }
    }

    let page_data_offset = page_lut_offset + page_lut.len() as u32;
    let glyph_table_offset = page_data_offset + page_data.len() as u32;

    file.write_all(b"TRBK")?;
    file.write_all(&[2u8])?; // version
    file.write_all(&[0u8])?; // flags
    file.write_all(&header_size.to_le_bytes())?;
    file.write_all(&options.screen_width.to_le_bytes())?;
    file.write_all(&options.screen_height.to_le_bytes())?;
    file.write_all(&page_count.to_le_bytes())?;
    file.write_all(&toc_count.to_le_bytes())?;
    file.write_all(&page_lut_offset.to_le_bytes())?;
    file.write_all(&toc_offset.to_le_bytes())?;
    file.write_all(&page_data_offset.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?; // embedded images offset
    file.write_all(&0u32.to_le_bytes())?; // source hash
    file.write_all(&glyph_count.to_le_bytes())?;
    file.write_all(&glyph_table_offset.to_le_bytes())?;

    file.write_all(&metadata_bytes)?;

    if toc_count != 0 {
        return Err(BookError::InvalidOutput);
    }

    file.write_all(&page_lut)?;
    file.write_all(&page_data)?;
    write_glyph_table(&mut file, glyphs)?;
    Ok(())
}

fn write_string<W: Write>(writer: &mut W, value: &str) -> Result<(), BookError> {
    let bytes = value.as_bytes();
    let len = bytes.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(bytes)?;
    Ok(())
}

fn output_path_for_size(base: &Path, size: u16, multi: bool) -> PathBuf {
    if !multi {
        return base.to_path_buf();
    }
    let mut stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "book".to_string());
    stem.push_str(&format!("-{}", size));
    let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("trbk");
    let mut out = base.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    out.push(format!("{}.{}", stem, ext));
    out
}

fn style_id_from_style(style: trusty_epub::TextStyle) -> StyleId {
    match (style.bold, style.italic) {
        (false, false) => StyleId::Regular,
        (true, false) => StyleId::Bold,
        (false, true) => StyleId::Italic,
        (true, true) => StyleId::BoldItalic,
    }
}

fn collect_used_codepoints(runs: &[trusty_epub::TextRun]) -> HashMap<StyleId, BTreeSet<u32>> {
    let mut map: HashMap<StyleId, BTreeSet<u32>> = HashMap::new();
    for run in runs {
        let style = style_id_from_style(run.style);
        let entry = map.entry(style).or_default();
        for ch in run.text.chars() {
            entry.insert(ch as u32);
        }
    }
    map
}

fn load_fonts(paths: &FontPaths) -> Result<HashMap<StyleId, fontdue::Font>, BookError> {
    let mut map = HashMap::new();
    let regular_path = paths
        .regular
        .as_deref()
        .unwrap_or("fonts/DejaVuSans.ttf");
    let regular_bytes = std::fs::read(regular_path).map_err(|err| {
        BookError::Io(std::io::Error::new(
            err.kind(),
            format!("missing font file: {regular_path}"),
        ))
    })?;
    let regular = fontdue::Font::from_bytes(regular_bytes, fontdue::FontSettings::default())
        .map_err(|_| BookError::InvalidOutput)?;
    map.insert(StyleId::Regular, regular.clone());

    if let Some(path) = paths.bold.as_deref() {
        let bytes = std::fs::read(path).map_err(|err| {
            BookError::Io(std::io::Error::new(
                err.kind(),
                format!("missing font file: {path}"),
            ))
        })?;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|_| BookError::InvalidOutput)?;
        map.insert(StyleId::Bold, font);
    }
    if let Some(path) = paths.italic.as_deref() {
        let bytes = std::fs::read(path).map_err(|err| {
            BookError::Io(std::io::Error::new(
                err.kind(),
                format!("missing font file: {path}"),
            ))
        })?;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|_| BookError::InvalidOutput)?;
        map.insert(StyleId::Italic, font);
    }
    if let Some(path) = paths.bold_italic.as_deref() {
        let bytes = std::fs::read(path).map_err(|err| {
            BookError::Io(std::io::Error::new(
                err.kind(),
                format!("missing font file: {path}"),
            ))
        })?;
        let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
            .map_err(|_| BookError::InvalidOutput)?;
        map.insert(StyleId::BoldItalic, font);
    }

    Ok(map)
}

fn build_glyphs(
    fonts: &HashMap<StyleId, fontdue::Font>,
    size: u16,
    used: &HashMap<StyleId, BTreeSet<u32>>,
) -> Result<Vec<Glyph>, BookError> {
    let mut glyphs = Vec::new();
    for (style, codepoints) in used {
        let font = fonts
            .get(style)
            .or_else(|| fonts.get(&StyleId::Regular))
            .ok_or(BookError::InvalidOutput)?;
        for codepoint in codepoints {
            if let Some(ch) = char::from_u32(*codepoint) {
                let (metrics, bitmap) = font.rasterize(ch, size as f32);
                let y_offset = (metrics.ymin + metrics.height as i32) as i16;
                let packed = pack_bitmap(&bitmap, metrics.width as usize, metrics.height as usize);
                glyphs.push(Glyph {
                    codepoint: *codepoint,
                    style: *style,
                    width: metrics.width as u8,
                    height: metrics.height as u8,
                    x_advance: metrics.advance_width.round() as i16,
                    x_offset: metrics.xmin as i16,
                    y_offset,
                    bitmap: packed,
                });
            }
        }
    }
    Ok(glyphs)
}

fn pack_bitmap(bitmap: &[u8], width: usize, height: usize) -> Vec<u8> {
    let total = width * height;
    let mut out = vec![0u8; (total + 7) / 8];
    for i in 0..total {
        let byte = i / 8;
        let bit = 7 - (i % 8);
        if bitmap[i] > 127 {
            out[byte] |= 1 << bit;
        }
    }
    out
}

fn write_glyph_table<W: Write>(writer: &mut W, glyphs: &[Glyph]) -> Result<(), BookError> {
    for glyph in glyphs {
        writer.write_all(&glyph.codepoint.to_le_bytes())?;
        writer.write_all(&[glyph.style as u8])?;
        writer.write_all(&[glyph.width])?;
        writer.write_all(&[glyph.height])?;
        writer.write_all(&glyph.x_advance.to_le_bytes())?;
        writer.write_all(&glyph.x_offset.to_le_bytes())?;
        writer.write_all(&glyph.y_offset.to_le_bytes())?;
        let len = glyph.bitmap.len() as u32;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&glyph.bitmap)?;
    }
    Ok(())
}
