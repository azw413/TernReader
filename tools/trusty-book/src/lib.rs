use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use image::GenericImageView;
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
    pub bitmap_bw: Vec<u8>,
    pub bitmap_lsb: Vec<u8>,
    pub bitmap_msb: Vec<u8>,
}

#[derive(Clone, Debug)]
struct SpineBlocks {
    spine_index: i32,
    blocks: Vec<trusty_epub::HtmlBlock>,
}

#[derive(Clone, Debug)]
enum LayoutItem {
    TextLine {
        spine_index: i32,
        runs: Vec<trusty_epub::TextRun>,
    },
    BlankLine {
        spine_index: i32,
    },
    Image {
        spine_index: i32,
        image_index: u16,
        width: u16,
        height: u16,
    },
    PageBreak {
        spine_index: i32,
    },
}

#[derive(Clone, Debug)]
struct PageData {
    spine_index: i32,
    ops: Vec<PageOp>,
}

#[derive(Clone, Debug)]
enum PageOp {
    Text {
        x: u16,
        y: u16,
        style: StyleId,
        text: String,
    },
    Image {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        image_index: u16,
    },
}

#[derive(Clone, Debug)]
struct ImageAsset {
    width: u16,
    height: u16,
    data: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct ImageRef {
    index: u16,
    width: u16,
    height: u16,
}

#[derive(Clone, Debug)]
struct TrbkTocEntry {
    title: String,
    page_index: u32,
    level: u8,
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

    let spine_blocks = extract_blocks(epub_path, &cache, 200)?;
    let used = collect_used_codepoints_from_blocks(&spine_blocks);
    let font_set = load_fonts(font_paths)?;
    warn_missing_style_fonts(&used, &font_set);

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
        let (image_assets, image_map) = build_image_assets(epub_path, &spine_blocks, &options)?;
        let items = layout_blocks(&spine_blocks, &options, &advance_map, &image_map);
        let pages = paginate_items(&items, &options, &advance_map);
        let spine_to_page = compute_spine_page_map(&pages, cache.spine.len());
        let toc_entries = build_toc_entries(&cache, &spine_to_page);
        write_trbk(
            &output,
            &metadata,
            &options,
            &pages,
            &glyphs,
            &toc_entries,
            &image_assets,
        )?;
    }

    Ok(())
}

fn extract_blocks(
    epub_path: &Path,
    cache: &trusty_epub::BookCache,
    max_spine_items: usize,
) -> Result<Vec<SpineBlocks>, BookError> {
    let mut out = Vec::new();
    let max_try = cache.spine.len().min(max_spine_items).max(1);
    let opf_dir = trusty_epub::opf_base_dir(&cache.opf_path);
    for index in 0..max_try {
        let xhtml = match trusty_epub::read_spine_xhtml(epub_path, index) {
            Ok(xhtml) => xhtml,
            Err(_) => continue,
        };
        let mut blocks = match trusty_epub::parse_xhtml_blocks(&xhtml) {
            Ok(blocks) => blocks,
            Err(_) => continue,
        };
        let spine_href = cache
            .spine
            .get(index)
            .map(|entry| entry.href.clone())
            .unwrap_or_default();
        let mut spine_path = strip_fragment(&spine_href);
        if spine_path.starts_with('/') {
            spine_path = spine_path.trim_start_matches('/').to_string();
        }
        let spine_path = if !opf_dir.is_empty() && spine_path.starts_with(&opf_dir) {
            spine_path
        } else {
            trusty_epub::resolve_href(&opf_dir, &spine_path)
        };
        let spine_path = collapse_double_prefix(&normalize_path(&spine_path), &opf_dir);
        let spine_dir = trusty_epub::opf_base_dir(&spine_path);
        for block in &mut blocks {
            if let trusty_epub::HtmlBlock::Image { src, .. } = block {
                let mut cleaned = strip_fragment(src);
                if cleaned.starts_with('/') {
                    cleaned = cleaned.trim_start_matches('/').to_string();
                }
                let resolved = if !opf_dir.is_empty() && cleaned.starts_with(&opf_dir) {
                    cleaned
                } else {
                    trusty_epub::resolve_href(&spine_dir, &cleaned)
                };
                let resolved = collapse_double_prefix(&normalize_path(&resolved), &opf_dir);
                *src = resolved;
            }
        }
        if !blocks.is_empty() {
            out.push(SpineBlocks {
                spine_index: index as i32,
                blocks,
            });
        }
        if out.len() > 500 {
            break;
        }
    }
    Ok(out)
}

fn collect_used_codepoints_from_blocks(
    blocks: &[SpineBlocks],
) -> HashMap<StyleId, BTreeSet<u32>> {
    let mut used: HashMap<StyleId, BTreeSet<u32>> = HashMap::new();
    for spine in blocks {
        for block in &spine.blocks {
            if let trusty_epub::HtmlBlock::Paragraph { runs, .. } = block {
                for run in runs {
                    let style = style_id_from_style(run.style);
                    let entry = used.entry(style).or_default();
                    for ch in run.text.chars() {
                        entry.insert(ch as u32);
                    }
                }
            }
        }
    }
    used
}

fn build_image_assets(
    epub_path: &Path,
    blocks: &[SpineBlocks],
    options: &RenderOptions,
) -> Result<(Vec<ImageAsset>, HashMap<String, ImageRef>), BookError> {
    let mut assets: Vec<ImageAsset> = Vec::new();
    let mut map: HashMap<String, ImageRef> = HashMap::new();

    for spine in blocks {
        for block in &spine.blocks {
            let trusty_epub::HtmlBlock::Image { src, .. } = block else {
                continue;
            };
            if map.contains_key(src) {
                continue;
            }
            let mut candidates = Vec::new();
            let mut candidate = strip_fragment(src);
            candidates.push(normalize_path(&candidate));
            let decoded = percent_decode(src);
            if decoded != *src {
                candidate = strip_fragment(&decoded);
                candidates.push(normalize_path(&candidate));
            }
            let mut bytes = None;
            for candidate in candidates.iter().filter(|c| !c.is_empty()) {
                match trusty_epub::read_epub_resource_bytes(epub_path, candidate) {
                    Ok(data) => {
                        bytes = Some(data);
                        break;
                    }
                    Err(_) => {}
                }
            }
            let Some(bytes) = bytes else {
                eprintln!("[trusty-book] warning: image not found in epub: {src}");
                continue;
            };
            let dyn_image = match image::load_from_memory(&bytes) {
                Ok(img) => img,
                Err(_) => {
                    eprintln!("[trusty-book] warning: failed to decode image: {src}");
                    continue;
                }
            };
            let (src_w, src_h) = dyn_image.dimensions();
            let max_w = options.screen_width.max(1) as u32;
            let max_h =
                (options.screen_height as i32 - options.margin_y as i32 * 2).max(1) as u32;
            let mut scale = if src_w >= max_w {
                max_w as f64 / src_w.max(1) as f64
            } else {
                let up = max_w as f64 / src_w.max(1) as f64;
                up.min(2.0)
            };
            let max_scale_h = max_h as f64 / src_h.max(1) as f64;
            if scale > max_scale_h {
                scale = max_scale_h;
            }
            let target_w = (src_w as f64 * scale).round().max(1.0) as u32;
            let target_h = (src_h as f64 * scale).round().max(1.0) as u32;
            let mut convert = trusty_image::ConvertOptions::default();
            convert.width = target_w;
            convert.height = target_h;
            convert.fit = trusty_image::FitMode::Contain;
            convert.dither = trusty_image::DitherMode::Bayer;
            convert.region_mode = trusty_image::RegionMode::None;
            convert.invert = false;
            convert.debug = false;
            convert.yolo_model = None;
            convert.trimg_version = 2;
            let trimg = trusty_image::convert_image(&dyn_image, convert);
            let data = trimg_to_bytes(&trimg);
            let index = assets.len() as u16;
            let image_ref = ImageRef {
                index,
                width: trimg.width as u16,
                height: trimg.height as u16,
            };
            assets.push(ImageAsset {
                width: image_ref.width,
                height: image_ref.height,
                data,
            });
            map.insert(src.clone(), image_ref);
        }
    }

    Ok((assets, map))
}

fn strip_fragment(path: &str) -> String {
    let mut end = path.len();
    for (idx, ch) in path.char_indices() {
        if ch == '#' || ch == '?' {
            end = idx;
            break;
        }
    }
    path[..end].to_string()
}

fn normalize_path(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if !parts.is_empty() {
                parts.pop();
            }
            continue;
        }
        parts.push(part);
    }
    parts.join("/")
}

fn collapse_double_prefix(path: &str, prefix: &str) -> String {
    if prefix.is_empty() {
        return path.to_string();
    }
    let prefix = prefix.trim_end_matches('/');
    let double = format!("{}/{}", prefix, prefix);
    if path.starts_with(&double) {
        format!("{}/{}", prefix, path[double.len()..].trim_start_matches('/'))
    } else {
        path.to_string()
    }
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = bytes[i + 1];
            let lo = bytes[i + 2];
            if let (Some(hi), Some(lo)) = (hex_val(hi), hex_val(lo)) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn layout_blocks(
    blocks: &[SpineBlocks],
    options: &RenderOptions,
    advance_map: &HashMap<(StyleId, u32), i16>,
    image_map: &HashMap<String, ImageRef>,
) -> Vec<LayoutItem> {
    let max_width = (options.screen_width as i32 - options.margin_x as i32 * 2).max(1);
    let mut items = Vec::new();
    for spine in blocks {
        let spine_index = spine.spine_index;
        for block in &spine.blocks {
            match block {
                trusty_epub::HtmlBlock::Paragraph { runs, .. } => {
                    let lines = wrap_paragraph_runs(runs, max_width, options, advance_map);
                    for line in lines {
                        items.push(LayoutItem::TextLine {
                            spine_index,
                            runs: line,
                        });
                    }
                    items.push(LayoutItem::BlankLine { spine_index });
                }
                trusty_epub::HtmlBlock::PageBreak => {
                    items.push(LayoutItem::PageBreak { spine_index });
                }
                trusty_epub::HtmlBlock::Image { src, .. } => {
                    if let Some(image) = image_map.get(src) {
                        items.push(LayoutItem::Image {
                            spine_index,
                            image_index: image.index,
                            width: image.width,
                            height: image.height,
                        });
                        items.push(LayoutItem::BlankLine { spine_index });
                    }
                }
            }
        }
    }
    items
}

fn wrap_paragraph_runs(
    runs: &[trusty_epub::TextRun],
    max_width: i32,
    options: &RenderOptions,
    advance_map: &HashMap<(StyleId, u32), i16>,
) -> Vec<Vec<trusty_epub::TextRun>> {
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
            lines.push(current);
            current = Vec::new();
            current.push(trusty_epub::TextRun {
                text: token.to_string(),
                style: run.style,
            });
            current_width = token_width;
        }
        if run.text.contains('\n') {
            if !current.is_empty() {
                lines.push(current);
                current = Vec::new();
                current_width = 0;
            }
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

fn paginate_items(
    items: &[LayoutItem],
    options: &RenderOptions,
    advance_map: &HashMap<(StyleId, u32), i16>,
) -> Vec<PageData> {
    let mut pages = Vec::new();
    let mut ops: Vec<PageOp> = Vec::new();
    let mut spine_index = -1i32;
    let mut cursor_y = options.margin_y as i32;
    let max_y = (options.screen_height as i32 - options.margin_y as i32).max(1);
    let line_height = options.line_height as i32;
    let image_spacing = (options.line_height as i32 / 2).max(0);

    let flush_page = |pages: &mut Vec<PageData>, ops: &mut Vec<PageOp>, spine_index: &mut i32, cursor_y: &mut i32| {
        if !ops.is_empty() {
            pages.push(PageData {
                spine_index: *spine_index,
                ops: core::mem::take(ops),
            });
            *spine_index = -1;
            *cursor_y = options.margin_y as i32;
        }
    };

    for item in items {
        let item_spine = match item {
            LayoutItem::TextLine { spine_index, .. } => *spine_index,
            LayoutItem::BlankLine { spine_index } => *spine_index,
            LayoutItem::Image { spine_index, .. } => *spine_index,
            LayoutItem::PageBreak { spine_index } => *spine_index,
        };

        if spine_index >= 0
            && item_spine >= 0
            && item_spine != spine_index
            && !ops.is_empty()
        {
            flush_page(&mut pages, &mut ops, &mut spine_index, &mut cursor_y);
        }

        if spine_index < 0 {
            spine_index = item_spine;
        }

        match item {
            LayoutItem::PageBreak { .. } => {
                flush_page(&mut pages, &mut ops, &mut spine_index, &mut cursor_y);
            }
            LayoutItem::BlankLine { .. } => {
                if cursor_y + line_height > max_y {
                    flush_page(&mut pages, &mut ops, &mut spine_index, &mut cursor_y);
                }
                cursor_y += line_height;
            }
            LayoutItem::TextLine { runs, .. } => {
                if cursor_y + line_height > max_y {
                    flush_page(&mut pages, &mut ops, &mut spine_index, &mut cursor_y);
                }
                let baseline = cursor_y + options.ascent as i32;
                let mut pen_x = options.margin_x as i32;
                for run in runs {
                    let style_id = style_id_from_style(run.style);
                    ops.push(PageOp::Text {
                        x: pen_x as u16,
                        y: baseline as u16,
                        style: style_id,
                        text: run.text.clone(),
                    });
                    let mut adv = measure_token_width(&run.text, run.style, options, advance_map);
                    if run.text == " " {
                        adv += options.word_spacing as i32;
                    }
                    pen_x += adv;
                }
                cursor_y += line_height;
            }
            LayoutItem::Image {
                image_index,
                width,
                height,
                ..
            } => {
                let img_h = *height as i32;
                if cursor_y + img_h > max_y {
                    flush_page(&mut pages, &mut ops, &mut spine_index, &mut cursor_y);
                }
                ops.push(PageOp::Image {
                    x: 0,
                    y: cursor_y as u16,
                    width: *width,
                    height: *height,
                    image_index: *image_index,
                });
                cursor_y += img_h + image_spacing;
            }
        }
    }

    if !ops.is_empty() {
        pages.push(PageData {
            spine_index,
            ops,
        });
    }
    if pages.is_empty() {
        pages.push(PageData {
            spine_index: -1,
            ops: vec![PageOp::Text {
                x: options.margin_x,
                y: (options.margin_y as i32 + options.ascent as i32) as u16,
                style: StyleId::Regular,
                text: "(empty)".to_string(),
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

fn warn_missing_style_fonts(
    used: &HashMap<StyleId, BTreeSet<u32>>,
    fonts: &HashMap<StyleId, fontdue::Font>,
) {
    let warn = |style: StyleId, label: &str| {
        if used.get(&style).map_or(false, |set| !set.is_empty()) && !fonts.contains_key(&style) {
            eprintln!(
                "[trusty-book] warning: {label} text found but no {label} font was loaded; using regular"
            );
        }
    };
    warn(StyleId::Bold, "bold");
    warn(StyleId::Italic, "italic");
    warn(StyleId::BoldItalic, "bold-italic");
}

fn compute_spine_page_map(pages: &[PageData], spine_count: usize) -> Vec<i32> {
    let mut map = vec![-1i32; spine_count];
    for (page_idx, page) in pages.iter().enumerate() {
        if page.spine_index >= 0 {
            let spine = page.spine_index as usize;
            if spine < map.len() && map[spine] < 0 {
                map[spine] = page_idx as i32;
            }
        }
    }
    map
}

fn build_toc_entries(
    cache: &trusty_epub::BookCache,
    spine_to_page: &[i32],
) -> Vec<TrbkTocEntry> {
    let mut entries = Vec::new();
    for entry in &cache.toc {
        if entry.spine_index < 0 {
            continue;
        }
        let spine = entry.spine_index as usize;
        if spine >= spine_to_page.len() {
            continue;
        }
        let page_index = spine_to_page[spine];
        if page_index < 0 {
            continue;
        }
        entries.push(TrbkTocEntry {
            title: entry.title.clone(),
            page_index: page_index as u32,
            level: entry.level,
        });
    }
    if entries.is_empty() {
        for (idx, spine) in cache.spine.iter().enumerate() {
            let page_index = spine_to_page.get(idx).copied().unwrap_or(-1);
            if page_index < 0 {
                continue;
            }
            let title = spine
                .href
                .split('/')
                .last()
                .unwrap_or("Chapter")
                .to_string();
            entries.push(TrbkTocEntry {
                title,
                page_index: page_index as u32,
                level: 0,
            });
        }
    }
    entries
}

fn write_trbk(
    path: &Path,
    metadata: &TrbkMetadata,
    options: &RenderOptions,
    pages: &[PageData],
    glyphs: &[Glyph],
    toc_entries: &[TrbkTocEntry],
    image_assets: &[ImageAsset],
) -> Result<(), BookError> {
    let mut file = File::create(path)?;

    let toc_count: u32 = toc_entries.len() as u32;
    let page_count = pages.len() as u32;
    let glyph_count = glyphs.len() as u32;
    let image_count = image_assets.len() as u32;

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
    let mut toc_bytes = Vec::new();
    for entry in toc_entries {
        write_string(&mut toc_bytes, &entry.title)?;
        toc_bytes.extend_from_slice(&entry.page_index.to_le_bytes());
        toc_bytes.push(entry.level);
        toc_bytes.push(0);
        toc_bytes.extend_from_slice(&0u16.to_le_bytes());
    }
    let page_lut_offset: u32 = toc_offset + toc_bytes.len() as u32;

    let mut page_lut = Vec::new();
    let mut page_data = Vec::new();

    for page in pages {
        let page_start = page_data.len() as u32;
        page_lut.extend_from_slice(&page_start.to_le_bytes());

        for op in &page.ops {
            match op {
                PageOp::Text { x, y, style, text } => {
                    let mut payload = Vec::new();
                    payload.extend_from_slice(&x.to_le_bytes());
                    payload.extend_from_slice(&y.to_le_bytes());
                    payload.push(*style as u8);
                    payload.push(0);
                    payload.extend_from_slice(text.as_bytes());
                    let length = payload.len() as u16;
                    page_data.push(0x01);
                    page_data.extend_from_slice(&length.to_le_bytes());
                    page_data.extend_from_slice(&payload);
                }
                PageOp::Image {
                    x,
                    y,
                    width,
                    height,
                    image_index,
                } => {
                    let mut payload = Vec::new();
                    payload.extend_from_slice(&x.to_le_bytes());
                    payload.extend_from_slice(&y.to_le_bytes());
                    payload.extend_from_slice(&width.to_le_bytes());
                    payload.extend_from_slice(&height.to_le_bytes());
                    payload.extend_from_slice(&image_index.to_le_bytes());
                    payload.extend_from_slice(&0u16.to_le_bytes());
                    let length = payload.len() as u16;
                    page_data.push(0x02);
                    page_data.extend_from_slice(&length.to_le_bytes());
                    page_data.extend_from_slice(&payload);
                }
            }
        }
    }

    let page_data_offset = page_lut_offset + page_lut.len() as u32;
    let glyph_table_offset = page_data_offset + page_data.len() as u32;
    let images_offset = if image_count > 0 {
        glyph_table_offset + glyphs_serialized_len(glyphs) as u32
    } else {
        0
    };

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
    file.write_all(&images_offset.to_le_bytes())?;
    file.write_all(&0u32.to_le_bytes())?; // source hash
    file.write_all(&glyph_count.to_le_bytes())?;
    file.write_all(&glyph_table_offset.to_le_bytes())?;

    file.write_all(&metadata_bytes)?;

    if toc_count != 0 {
        file.write_all(&toc_bytes)?;
    }
    file.write_all(&page_lut)?;
    file.write_all(&page_data)?;
    write_glyph_table(&mut file, glyphs)?;
    if image_count > 0 {
        write_image_table(&mut file, image_assets)?;
    }
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

    let auto_bold = if paths.bold.is_none() {
        guess_font_variant(regular_path, FontVariant::Bold)
    } else {
        None
    };
    let auto_italic = if paths.italic.is_none() {
        guess_font_variant(regular_path, FontVariant::Italic)
    } else {
        None
    };
    let auto_bold_italic = if paths.bold_italic.is_none() {
        guess_font_variant(regular_path, FontVariant::BoldItalic)
    } else {
        None
    };

    if let Some(path) = paths.bold.as_deref().or(auto_bold.as_deref()) {
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
    if let Some(path) = paths.italic.as_deref().or(auto_italic.as_deref()) {
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
    if let Some(path) = paths.bold_italic.as_deref().or(auto_bold_italic.as_deref()) {
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

#[derive(Clone, Copy, Debug)]
enum FontVariant {
    Bold,
    Italic,
    BoldItalic,
}

fn guess_font_variant(regular_path: &str, variant: FontVariant) -> Option<String> {
    let path = Path::new(regular_path);
    let stem = path.file_stem()?.to_string_lossy();
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("ttf");
    let mut candidates = Vec::new();

    // Common patterns: Foo-Regular -> Foo-Bold / Foo-Italic / Foo-BoldItalic
    let base = stem
        .replace("Regular", "")
        .replace("regular", "")
        .replace("Roman", "")
        .replace("roman", "")
        .trim_end_matches('-')
        .trim_end_matches('_')
        .to_string();
    let suffix = match variant {
        FontVariant::Bold => "Bold",
        FontVariant::Italic => "Italic",
        FontVariant::BoldItalic => "Bold Italic",
    };
    if !base.is_empty() {
        candidates.push(format!("{}-{}.{}", base, suffix, ext));
        candidates.push(format!("{}_{}.{}", base, suffix, ext));
        candidates.push(format!("{} {}.{}", base, suffix, ext));
        candidates.push(format!("{}{}.{}", base, suffix.replace(' ', ""), ext));
    }
    // Also try replacing Regular in the original stem.
    let replaced = match variant {
        FontVariant::Bold => stem.replace("Regular", "Bold").replace("regular", "Bold"),
        FontVariant::Italic => stem.replace("Regular", "Italic").replace("regular", "Italic"),
        FontVariant::BoldItalic => stem
            .replace("Regular", "Bold Italic")
            .replace("regular", "Bold Italic"),
    };
    if replaced != stem {
        candidates.push(format!("{}.{}", replaced, ext));
    }

    for name in candidates {
        let candidate = path.with_file_name(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
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
                let (bw, lsb, msb) =
                    pack_gray2_bitmap(&bitmap, metrics.width as usize, metrics.height as usize);
                glyphs.push(Glyph {
                    codepoint: *codepoint,
                    style: *style,
                    width: metrics.width as u8,
                    height: metrics.height as u8,
                    x_advance: metrics.advance_width.round() as i16,
                    x_offset: metrics.xmin as i16,
                    y_offset,
                    bitmap_bw: bw,
                    bitmap_lsb: lsb,
                    bitmap_msb: msb,
                });
            }
        }
    }
    Ok(glyphs)
}

fn pack_gray2_bitmap(bitmap: &[u8], width: usize, height: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let total = width * height;
    let mut bw = vec![0u8; (total + 7) / 8];
    let mut lsb = vec![0u8; (total + 7) / 8];
    let mut msb = vec![0u8; (total + 7) / 8];
    for i in 0..total {
        let byte = i / 8;
        let bit = 7 - (i % 8);
        let val = 255u8.saturating_sub(bitmap[i]);
        let (bw_bit, msb_bit, lsb_bit) = if val >= 205 {
            (1u8, 0u8, 0u8)
        } else if val >= 154 {
            (1u8, 0u8, 1u8)
        } else if val >= 103 {
            (0u8, 1u8, 0u8)
        } else if val >= 52 {
            (0u8, 1u8, 1u8)
        } else {
            (0u8, 0u8, 0u8)
        };
        if bw_bit != 0 {
            bw[byte] |= 1 << bit;
        }
        if lsb_bit != 0 {
            lsb[byte] |= 1 << bit;
        }
        if msb_bit != 0 {
            msb[byte] |= 1 << bit;
        }
    }
    (bw, lsb, msb)
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
        let len = (glyph.bitmap_bw.len() + glyph.bitmap_lsb.len() + glyph.bitmap_msb.len()) as u32;
        writer.write_all(&len.to_le_bytes())?;
        writer.write_all(&glyph.bitmap_bw)?;
        writer.write_all(&glyph.bitmap_lsb)?;
        writer.write_all(&glyph.bitmap_msb)?;
    }
    Ok(())
}

fn glyphs_serialized_len(glyphs: &[Glyph]) -> usize {
    let mut total = 0usize;
    for glyph in glyphs {
        total += 4
            + 1
            + 1
            + 1
            + 2
            + 2
            + 2
            + 4
            + glyph.bitmap_bw.len()
            + glyph.bitmap_lsb.len()
            + glyph.bitmap_msb.len();
    }
    total
}

fn write_image_table<W: Write>(writer: &mut W, images: &[ImageAsset]) -> Result<(), BookError> {
    let count = images.len() as u32;
    let table_size = 4 + images.len() * 16;
    let mut data_offset = table_size as u32;

    writer.write_all(&count.to_le_bytes())?;
    for image in images {
        writer.write_all(&data_offset.to_le_bytes())?;
        writer.write_all(&(image.data.len() as u32).to_le_bytes())?;
        writer.write_all(&image.width.to_le_bytes())?;
        writer.write_all(&image.height.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        data_offset = data_offset.saturating_add(image.data.len() as u32);
    }
    for image in images {
        writer.write_all(&image.data)?;
    }
    Ok(())
}

fn trimg_to_bytes(trimg: &trusty_image::Trimg) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"TRIM");
    match &trimg.data {
        trusty_image::TrimgData::Mono1 { bits } => {
            out.push(1);
            out.push(1);
            out.extend_from_slice(&(trimg.width as u16).to_le_bytes());
            out.extend_from_slice(&(trimg.height as u16).to_le_bytes());
            out.extend_from_slice(&[0u8; 6]);
            out.extend_from_slice(bits);
        }
        trusty_image::TrimgData::Gray2 { data } => {
            out.push(2);
            out.push(2);
            out.extend_from_slice(&(trimg.width as u16).to_le_bytes());
            out.extend_from_slice(&(trimg.height as u16).to_le_bytes());
            out.extend_from_slice(&[0u8; 6]);
            out.extend_from_slice(data);
        }
    }
    out
}
