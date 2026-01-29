use std::collections::{HashMap, VecDeque};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EpubError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("xml error: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("utf8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("container.xml missing rootfile")]
    MissingRootfile,
    #[error("opf package missing")]
    MissingPackage,
    #[error("spine index out of range")]
    InvalidSpineIndex,
}

#[derive(Debug, Clone)]
pub struct EpubContainer {
    pub rootfile_path: String,
}

#[derive(Debug, Clone, Default)]
pub struct OpfMetadata {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub language: Option<String>,
    pub identifier: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OpfManifestItem {
    pub id: String,
    pub href: String,
    pub media_type: String,
    pub properties: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OpfSpineItem {
    pub idref: String,
    pub linear: bool,
}

#[derive(Debug, Clone)]
pub struct OpfPackage {
    pub metadata: OpfMetadata,
    pub manifest: Vec<OpfManifestItem>,
    pub spine: Vec<OpfSpineItem>,
    pub nav_href: Option<String>,
    pub toc_href: Option<String>,
    pub cover_href: Option<String>,
    pub opf_path: String,
    pub opf_dir: String,
}

#[derive(Debug, Clone)]
pub struct TocEntry {
    pub label: String,
    pub href: String,
    pub children: Vec<TocEntry>,
}

#[derive(Debug, Clone)]
pub struct EpubBook {
    pub container: EpubContainer,
    pub package: OpfPackage,
    pub toc: Vec<TocEntry>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextStyle {
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone)]
pub struct TextRun {
    pub text: String,
    pub style: TextStyle,
}

#[derive(Debug, Clone)]
pub enum HtmlBlock {
    Paragraph {
        runs: Vec<TextRun>,
        heading_level: Option<u8>,
    },
    PageBreak,
    ImagePlaceholder { alt: Option<String> },
}

#[derive(Debug, Clone)]
pub struct CacheSpineEntry {
    pub href: String,
    pub cumulative_size: u64,
    pub toc_index: i32,
}

#[derive(Debug, Clone)]
pub struct CacheTocEntry {
    pub title: String,
    pub href: String,
    pub anchor: String,
    pub level: u8,
    pub spine_index: i32,
}

#[derive(Debug, Clone)]
pub struct BookCache {
    pub metadata: OpfMetadata,
    pub opf_path: String,
    pub cover_href: Option<String>,
    pub spine: Vec<CacheSpineEntry>,
    pub toc: Vec<CacheTocEntry>,
    pub cache_path: PathBuf,
    pub source_size: u64,
    pub source_mtime: u64,
}

#[derive(Debug, Clone)]
pub struct CacheStatus {
    pub hit: bool,
    pub cache_path: PathBuf,
}

const CACHE_VERSION: u8 = 1;

pub fn open_epub<P: AsRef<Path>>(path: P) -> Result<EpubBook, EpubError> {
    let file = std::fs::File::open(path.as_ref())?;
    let mut archive = zip::ZipArchive::new(file)?;

    let container_xml = read_zip_file_to_string(&mut archive, "META-INF/container.xml")?;
    let container = parse_container(&container_xml)?;

    let opf_xml = read_zip_file_to_string(&mut archive, &container.rootfile_path)?;
    let mut package = parse_opf(&opf_xml, &container.rootfile_path)?;

    let toc = if let Some(nav_href) = package.nav_href.clone() {
        let nav_path = resolve_href(&package.opf_dir, &nav_href);
        let nav_xml = read_zip_file_to_string(&mut archive, &nav_path)?;
        match parse_nav_toc(&nav_xml, &nav_path) {
            Ok(toc) => toc,
            Err(_) => Vec::new(),
        }
    } else if let Some(toc_href) = package.toc_href.clone() {
        let toc_path = resolve_href(&package.opf_dir, &toc_href);
        let toc_xml = read_zip_file_to_string(&mut archive, &toc_path)?;
        parse_ncx_toc(&toc_xml, &toc_path)?
    } else {
        Vec::new()
    };

    if package.cover_href.is_none() {
        package.cover_href = find_cover_href(&package);
    }

    Ok(EpubBook {
        container,
        package,
        toc,
    })
}

pub fn parse_xhtml_blocks(xml: &str) -> Result<Vec<HtmlBlock>, EpubError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut buf = Vec::new();
    let mut blocks: Vec<HtmlBlock> = Vec::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current_text = String::new();
    let mut current_style = TextStyle::default();
    let mut heading_level: Option<u8> = None;
    let mut in_body = true;
    let mut skip_depth: usize = 0;
    let mut last_was_space = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name_buf = e.name().as_ref().to_vec();
                let name = name_buf.as_slice();
                if is_xml_name(name, b"body") {
                    in_body = true;
                }
                if is_xml_name(name, b"head") {
                    skip_depth = 1;
                } else if skip_depth > 0 {
                    skip_depth += 1;
                }
                if !in_body || skip_depth > 0 {
                    buf.clear();
                    continue;
                }

                if is_block_tag(name) {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    heading_level = heading_level_from(name);
                    last_was_space = false;
                } else if is_xml_name(name, b"br") {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    heading_level = None;
                    last_was_space = false;
                } else if is_xml_name(name, b"img") {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    let alt = attr_value(&e, b"alt")?;
                    blocks.push(HtmlBlock::ImagePlaceholder { alt });
                    heading_level = None;
                    last_was_space = false;
                } else if is_xml_name(name, b"b") || is_xml_name(name, b"strong") {
                    flush_text_run(&mut runs, &mut current_text, current_style, &mut last_was_space);
                    current_style.bold = true;
                } else if is_xml_name(name, b"i") || is_xml_name(name, b"em") {
                    flush_text_run(&mut runs, &mut current_text, current_style, &mut last_was_space);
                    current_style.italic = true;
                } else if is_pagebreak(&e)? {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    blocks.push(HtmlBlock::PageBreak);
                    heading_level = None;
                    last_was_space = false;
                }
            }
            Event::Empty(e) => {
                let name_buf = e.name().as_ref().to_vec();
                let name = name_buf.as_slice();
                if is_xml_name(name, b"br") {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    heading_level = None;
                    last_was_space = false;
                } else if is_xml_name(name, b"img") {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    let alt = attr_value(&e, b"alt")?;
                    blocks.push(HtmlBlock::ImagePlaceholder { alt });
                    heading_level = None;
                    last_was_space = false;
                } else if is_pagebreak(&e)? {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    blocks.push(HtmlBlock::PageBreak);
                    heading_level = None;
                    last_was_space = false;
                }
            }
            Event::End(e) => {
                let name_buf = e.name().as_ref().to_vec();
                let name = name_buf.as_slice();
                if is_xml_name(name, b"head") && skip_depth > 0 {
                    skip_depth = skip_depth.saturating_sub(1);
                } else if skip_depth > 0 {
                    skip_depth = skip_depth.saturating_sub(1);
                }
                if !in_body || skip_depth > 0 {
                    buf.clear();
                    continue;
                }

                if is_block_tag(name) {
                    flush_paragraph(
                        &mut blocks,
                        &mut runs,
                        &mut current_text,
                        current_style,
                        heading_level,
                    );
                    heading_level = None;
                    last_was_space = false;
                } else if is_xml_name(name, b"b") || is_xml_name(name, b"strong") {
                    flush_text_run(&mut runs, &mut current_text, current_style, &mut last_was_space);
                    current_style.bold = false;
                } else if is_xml_name(name, b"i") || is_xml_name(name, b"em") {
                    flush_text_run(&mut runs, &mut current_text, current_style, &mut last_was_space);
                    current_style.italic = false;
                } else if is_xml_name(name, b"body") {
                    in_body = false;
                }
            }
            Event::Text(e) => {
                if !in_body || skip_depth > 0 {
                    buf.clear();
                    continue;
                }
                let decoded = e.decode().map_err(quick_xml::Error::from)?;
                push_normalized_text(
                    &decoded,
                    &mut current_text,
                    &mut last_was_space,
                );
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    flush_paragraph(
        &mut blocks,
        &mut runs,
        &mut current_text,
        current_style,
        heading_level,
    );
    Ok(blocks)
}

pub fn read_spine_xhtml<P: AsRef<Path>>(epub_path: P, spine_index: usize) -> Result<String, EpubError> {
    let epub_path = epub_path.as_ref();
    let book = open_epub(epub_path)?;
    let spine_hrefs = build_spine_hrefs(&book.package);
    let href = spine_hrefs
        .get(spine_index)
        .ok_or(EpubError::InvalidSpineIndex)?;
    let file = std::fs::File::open(epub_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    read_zip_file_to_string(&mut archive, href)
}

pub fn blocks_to_plain_text(blocks: &[HtmlBlock]) -> String {
    let mut out = String::new();
    for (idx, block) in blocks.iter().enumerate() {
        match block {
            HtmlBlock::Paragraph { runs, .. } => {
                if idx > 0 && !out.ends_with('\n') {
                    out.push('\n');
                }
                let mut line = String::new();
                for run in runs {
                    line.push_str(&run.text);
                }
                out.push_str(line.trim());
                out.push('\n');
                out.push('\n');
            }
            HtmlBlock::PageBreak => {
                out.push_str("\n\n");
            }
            HtmlBlock::ImagePlaceholder { alt } => {
                let label = alt.as_deref().unwrap_or("image");
                out.push_str(&format!("[Image: {label}]\n\n"));
            }
        }
    }
    out
}

pub fn blocks_to_runs(blocks: &[HtmlBlock]) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut first = true;
    for block in blocks {
        match block {
            HtmlBlock::Paragraph { runs: para_runs, .. } => {
                if !first {
                    runs.push(TextRun {
                        text: "\n\n".to_string(),
                        style: TextStyle::default(),
                    });
                }
                first = false;
                for run in para_runs {
                    if !run.text.is_empty() {
                        runs.push(run.clone());
                    }
                }
            }
            HtmlBlock::PageBreak => {
                runs.push(TextRun {
                    text: "\n\n".to_string(),
                    style: TextStyle::default(),
                });
            }
            HtmlBlock::ImagePlaceholder { .. } => {
                // Skip images for text runs.
            }
        }
    }
    runs
}

pub fn default_cache_dir<P: AsRef<Path>>(epub_path: P) -> PathBuf {
    let path = epub_path.as_ref();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "book".to_string());
    let mut dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    dir.push(".trusty_epub_cache");
    dir.push(stem);
    dir
}

pub fn load_or_build_cache<P: AsRef<Path>, Q: AsRef<Path>>(
    epub_path: P,
    cache_dir: Q,
) -> Result<(BookCache, CacheStatus), EpubError> {
    let cache_path = cache_dir.as_ref().join("book.bin");
    if let Some(cache) = load_cache(epub_path.as_ref(), &cache_path)? {
        return Ok((
            cache,
            CacheStatus {
                hit: true,
                cache_path,
            },
        ));
    }

    let cache = build_cache(epub_path.as_ref(), cache_dir.as_ref())?;
    Ok((
        cache,
        CacheStatus {
            hit: false,
            cache_path,
        },
    ))
}

pub fn load_cache(epub_path: &Path, cache_path: &Path) -> Result<Option<BookCache>, EpubError> {
    let meta = std::fs::metadata(epub_path)?;
    let source_size = meta.len();
    let source_mtime = system_time_secs(meta.modified().ok());

    let mut file = match std::fs::File::open(cache_path) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };

    let version = read_u8(&mut file)?;
    if version != CACHE_VERSION {
        return Ok(None);
    }

    let cached_size = read_u64(&mut file)?;
    let cached_mtime = read_u64(&mut file)?;
    if cached_size != source_size || cached_mtime != source_mtime {
        return Ok(None);
    }

    let spine_count = read_u32(&mut file)? as usize;
    let toc_count = read_u32(&mut file)? as usize;

    let title = read_string(&mut file)?;
    let creator = read_string(&mut file)?;
    let language = read_string(&mut file)?;
    let identifier = read_string(&mut file)?;
    let cover_href = read_string(&mut file)?;
    let opf_path = read_string(&mut file)?;

    let mut spine = Vec::with_capacity(spine_count);
    for _ in 0..spine_count {
        let href = read_string(&mut file)?;
        let cumulative_size = read_u64(&mut file)?;
        let toc_index = read_i32(&mut file)?;
        spine.push(CacheSpineEntry {
            href,
            cumulative_size,
            toc_index,
        });
    }

    let mut toc = Vec::with_capacity(toc_count);
    for _ in 0..toc_count {
        let title = read_string(&mut file)?;
        let href = read_string(&mut file)?;
        let anchor = read_string(&mut file)?;
        let level = read_u8(&mut file)?;
        let spine_index = read_i32(&mut file)?;
        toc.push(CacheTocEntry {
            title,
            href,
            anchor,
            level,
            spine_index,
        });
    }

    Ok(Some(BookCache {
        metadata: OpfMetadata {
            title: if title.is_empty() { None } else { Some(title) },
            creator: if creator.is_empty() { None } else { Some(creator) },
            language: if language.is_empty() { None } else { Some(language) },
            identifier: if identifier.is_empty() { None } else { Some(identifier) },
        },
        opf_path,
        cover_href: if cover_href.is_empty() {
            None
        } else {
            Some(cover_href)
        },
        spine,
        toc,
        cache_path: cache_path.to_path_buf(),
        source_size,
        source_mtime,
    }))
}

pub fn build_cache(epub_path: &Path, cache_dir: &Path) -> Result<BookCache, EpubError> {
    std::fs::create_dir_all(cache_dir)?;

    let meta = std::fs::metadata(epub_path)?;
    let source_size = meta.len();
    let source_mtime = system_time_secs(meta.modified().ok());

    let book = open_epub(epub_path)?;
    let spine_hrefs = build_spine_hrefs(&book.package);

    let mut archive = zip::ZipArchive::new(std::fs::File::open(epub_path)?)?;
    let mut spine_entries = Vec::with_capacity(spine_hrefs.len());
    let mut cumulative_size = 0u64;

    for href in &spine_hrefs {
        let size = zip_entry_size(&mut archive, href).unwrap_or(0);
        cumulative_size = cumulative_size.saturating_add(size);
        spine_entries.push(CacheSpineEntry {
            href: href.clone(),
            cumulative_size,
            toc_index: -1,
        });
    }

    let mut href_to_index = HashMap::new();
    for (idx, href) in spine_hrefs.iter().enumerate() {
        href_to_index.insert(href.as_str(), idx as i32);
    }

    let mut toc_entries = Vec::new();
    flatten_toc(&book.toc, 0, &mut toc_entries, &href_to_index);

    for (idx, entry) in toc_entries.iter().enumerate() {
        if entry.spine_index >= 0 && (entry.spine_index as usize) < spine_entries.len() {
            spine_entries[entry.spine_index as usize].toc_index = idx as i32;
        }
    }

    let cache_path = cache_dir.join("book.bin");
    let mut file = std::fs::File::create(&cache_path)?;

    write_u8(&mut file, CACHE_VERSION)?;
    write_u64(&mut file, source_size)?;
    write_u64(&mut file, source_mtime)?;
    write_u32(&mut file, spine_entries.len() as u32)?;
    write_u32(&mut file, toc_entries.len() as u32)?;

    write_string(&mut file, book.package.metadata.title.as_deref().unwrap_or(""))?;
    write_string(
        &mut file,
        book.package.metadata.creator.as_deref().unwrap_or(""),
    )?;
    write_string(
        &mut file,
        book.package.metadata.language.as_deref().unwrap_or(""),
    )?;
    write_string(
        &mut file,
        book.package.metadata.identifier.as_deref().unwrap_or(""),
    )?;
    write_string(&mut file, book.package.cover_href.as_deref().unwrap_or(""))?;
    write_string(&mut file, &book.package.opf_path)?;

    for entry in &spine_entries {
        write_string(&mut file, &entry.href)?;
        write_u64(&mut file, entry.cumulative_size)?;
        write_i32(&mut file, entry.toc_index)?;
    }

    for entry in &toc_entries {
        write_string(&mut file, &entry.title)?;
        write_string(&mut file, &entry.href)?;
        write_string(&mut file, &entry.anchor)?;
        write_u8(&mut file, entry.level)?;
        write_i32(&mut file, entry.spine_index)?;
    }

    Ok(BookCache {
        metadata: book.package.metadata,
        opf_path: book.package.opf_path,
        cover_href: book.package.cover_href,
        spine: spine_entries,
        toc: toc_entries,
        cache_path,
        source_size,
        source_mtime,
    })
}

fn read_zip_file_to_string<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    path: &str,
) -> Result<String, EpubError> {
    let mut file = archive.by_name(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8(buf)?)
}

fn parse_container(xml: &str) -> Result<EpubContainer, EpubError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut rootfile_path: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) | Event::Empty(e) => {
                if is_xml_name(e.name().as_ref(), b"rootfile") {
                    if let Some(path) = attr_value(&e, b"full-path")? {
                        rootfile_path = Some(path);
                        break;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    let rootfile_path = rootfile_path.ok_or(EpubError::MissingRootfile)?;
    Ok(EpubContainer { rootfile_path })
}

fn parse_opf(xml: &str, opf_path: &str) -> Result<OpfPackage, EpubError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let opf_dir = opf_base_dir(opf_path);

    let mut buf = Vec::new();
    let mut in_metadata = false;
    let mut in_manifest = false;
    let mut in_spine = false;
    let mut current_meta: Option<&'static str> = None;

    let mut metadata = OpfMetadata::default();
    let mut manifest = Vec::new();
    let mut spine = Vec::new();
    let mut nav_href = None;
    let mut toc_href = None;
    let mut cover_id = None;
    let mut spine_toc_id: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                match e.name().as_ref() {
                    name if is_xml_name(name, b"metadata") => in_metadata = true,
                    name if is_xml_name(name, b"manifest") => in_manifest = true,
                    name if is_xml_name(name, b"spine") => {
                        in_spine = true;
                        if let Some(toc) = attr_value(&e, b"toc")? {
                            spine_toc_id = Some(toc);
                        }
                    }
                    name if is_xml_name(name, b"item") && in_manifest => {
                        let id = attr_value(&e, b"id")?.unwrap_or_default();
                        let href = attr_value(&e, b"href")?.unwrap_or_default();
                        let media_type = attr_value(&e, b"media-type")?.unwrap_or_default();
                        let properties = attr_value(&e, b"properties")?.unwrap_or_default();
                        let properties = properties
                            .split_whitespace()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>();

                        if properties.iter().any(|p| p == "nav") {
                            nav_href = Some(href.clone());
                        }

                        manifest.push(OpfManifestItem {
                            id,
                            href,
                            media_type,
                            properties,
                        });
                    }
                    name if is_xml_name(name, b"itemref") && in_spine => {
                        let idref = attr_value(&e, b"idref")?.unwrap_or_default();
                        let linear = match attr_value(&e, b"linear")? {
                            Some(v) => v != "no",
                            None => true,
                        };
                        spine.push(OpfSpineItem { idref, linear });
                    }
                    name if is_xml_name(name, b"meta") && in_metadata => {
                        let name = attr_value(&e, b"name")?;
                        let property = attr_value(&e, b"property")?;
                        let content = attr_value(&e, b"content")?;
                        if let Some(name) = name {
                            if name == "cover" {
                                cover_id = content.clone();
                            }
                        }
                        if let Some(property) = property {
                            if property == "cover-image" {
                                cover_id = content;
                            }
                        }
                    }
                    name if in_metadata && is_xml_name(name, b"title") => {
                        current_meta = Some("title");
                    }
                    name if in_metadata && is_xml_name(name, b"creator") => {
                        current_meta = Some("creator");
                    }
                    name if in_metadata && is_xml_name(name, b"language") => {
                        current_meta = Some("language");
                    }
                    name if in_metadata && is_xml_name(name, b"identifier") => {
                        current_meta = Some("identifier");
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => match e.name().as_ref() {
                name if is_xml_name(name, b"item") && in_manifest => {
                    let id = attr_value(&e, b"id")?.unwrap_or_default();
                    let href = attr_value(&e, b"href")?.unwrap_or_default();
                    let media_type = attr_value(&e, b"media-type")?.unwrap_or_default();
                    let properties = attr_value(&e, b"properties")?.unwrap_or_default();
                    let properties = properties
                        .split_whitespace()
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>();

                    if properties.iter().any(|p| p == "nav") {
                        nav_href = Some(href.clone());
                    }

                    manifest.push(OpfManifestItem {
                        id,
                        href,
                        media_type,
                        properties,
                    });
                }
                name if is_xml_name(name, b"itemref") && in_spine => {
                    let idref = attr_value(&e, b"idref")?.unwrap_or_default();
                    let linear = match attr_value(&e, b"linear")? {
                        Some(v) => v != "no",
                        None => true,
                    };
                    spine.push(OpfSpineItem { idref, linear });
                }
                name if is_xml_name(name, b"meta") && in_metadata => {
                    let name = attr_value(&e, b"name")?;
                    let property = attr_value(&e, b"property")?;
                    let content = attr_value(&e, b"content")?;
                    if let Some(name) = name {
                        if name == "cover" {
                            cover_id = content.clone();
                        }
                    }
                    if let Some(property) = property {
                        if property == "cover-image" {
                            cover_id = content;
                        }
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                name if is_xml_name(name, b"metadata") => in_metadata = false,
                name if is_xml_name(name, b"manifest") => in_manifest = false,
                name if is_xml_name(name, b"spine") => in_spine = false,
                name
                    if is_xml_name(name, b"title")
                        || is_xml_name(name, b"creator")
                        || is_xml_name(name, b"language")
                        || is_xml_name(name, b"identifier") =>
                {
                    current_meta = None;
                }
                _ => {}
            },
            Event::Text(e) => {
                if let Some(field) = current_meta {
                    let text = e.decode().map_err(quick_xml::Error::from)?.into_owned();
                    if !text.is_empty() {
                        match field {
                            "title" => metadata.title = Some(text),
                            "creator" => metadata.creator = Some(text),
                            "language" => metadata.language = Some(text),
                            "identifier" => metadata.identifier = Some(text),
                            _ => {}
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    if let Some(toc_id) = spine_toc_id {
        if let Some(item) = manifest.iter().find(|item| item.id == toc_id) {
            toc_href = Some(item.href.clone());
        }
    }

    let cover_href = cover_id.and_then(|id| {
        manifest
            .iter()
            .find(|item| item.id == id)
            .map(|item| item.href.clone())
    });

    Ok(OpfPackage {
        metadata,
        manifest,
        spine,
        nav_href,
        toc_href,
        cover_href,
        opf_path: opf_path.to_string(),
        opf_dir,
    })
}

fn parse_nav_toc(xml: &str, nav_path: &str) -> Result<Vec<TocEntry>, EpubError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let base_dir = opf_base_dir(nav_path);

    let mut buf = Vec::new();
    let mut toc: Vec<TocEntry> = Vec::new();
    let mut stack: Vec<TocEntry> = Vec::new();
    let mut in_toc_nav = false;
    let mut nav_depth = 0usize;
    let mut in_link = false;
    let mut current_href: Option<String> = None;
    let mut current_text = String::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                match e.name().as_ref() {
                    b"nav" => {
                        if is_toc_nav(&e)? {
                            in_toc_nav = true;
                            nav_depth = 1;
                        } else if in_toc_nav {
                            nav_depth += 1;
                        }
                    }
                    b"li" if in_toc_nav => {
                        stack.push(TocEntry {
                            label: String::new(),
                            href: String::new(),
                            children: Vec::new(),
                        });
                    }
                    b"a" if in_toc_nav => {
                        in_link = true;
                        current_text.clear();
                        current_href = attr_value(&e, b"href")?;
                    }
                    _ => {}
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"nav" if in_toc_nav => {
                    if nav_depth == 1 {
                        in_toc_nav = false;
                    } else {
                        nav_depth -= 1;
                    }
                }
                b"a" if in_toc_nav => {
                    in_link = false;
                    if let Some(entry) = stack.last_mut() {
                        entry.label = current_text.trim().to_string();
                        if let Some(href) = current_href.take() {
                            entry.href = resolve_href(&base_dir, &href);
                        }
                    }
                }
                b"li" if in_toc_nav => {
                    if let Some(entry) = stack.pop() {
                        if let Some(parent) = stack.last_mut() {
                            parent.children.push(entry);
                        } else {
                            toc.push(entry);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(e) => {
                if in_toc_nav && in_link {
                    current_text.push_str(&e.decode().map_err(quick_xml::Error::from)?.into_owned());
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(toc)
}

fn parse_ncx_toc(xml: &str, ncx_path: &str) -> Result<Vec<TocEntry>, EpubError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let base_dir = opf_base_dir(ncx_path);

    let mut buf = Vec::new();
    let mut toc: Vec<TocEntry> = Vec::new();
    let mut stack: VecDeque<TocEntry> = VecDeque::new();
    let mut in_nav_label = false;
    let mut in_label_text = false;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => match e.name().as_ref() {
                b"navPoint" => {
                    stack.push_back(TocEntry {
                        label: String::new(),
                        href: String::new(),
                        children: Vec::new(),
                    });
                }
                b"navLabel" => in_nav_label = true,
                b"text" if in_nav_label => in_label_text = true,
                b"content" => {
                    if let Some(href) = attr_value(&e, b"src")? {
                        if let Some(entry) = stack.back_mut() {
                            entry.href = resolve_href(&base_dir, &href);
                        }
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.name().as_ref() {
                b"navLabel" => in_nav_label = false,
                b"text" => in_label_text = false,
                b"navPoint" => {
                    if let Some(entry) = stack.pop_back() {
                        if let Some(parent) = stack.back_mut() {
                            parent.children.push(entry);
                        } else {
                            toc.push(entry);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(e) => {
                if in_nav_label && in_label_text {
                    if let Some(entry) = stack.back_mut() {
                        entry.label = e.decode().map_err(quick_xml::Error::from)?.into_owned();
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(toc)
}

fn find_cover_href(package: &OpfPackage) -> Option<String> {
    package
        .manifest
        .iter()
        .find(|item| item.properties.iter().any(|p| p == "cover-image"))
        .map(|item| item.href.clone())
}

fn is_toc_nav(e: &BytesStart<'_>) -> Result<bool, EpubError> {
    let mut is_toc = false;
    if let Some(value) = attr_value(e, b"epub:type")? {
        if value == "toc" {
            is_toc = true;
        }
    }
    if let Some(value) = attr_value(e, b"type")? {
        if value == "toc" {
            is_toc = true;
        }
    }
    Ok(is_toc)
}

fn attr_value(e: &BytesStart<'_>, name: &[u8]) -> Result<Option<String>, EpubError> {
    for attr in e.attributes().with_checks(false) {
        let attr = attr.map_err(quick_xml::Error::from)?;
        if attr.key.as_ref() == name {
            return Ok(Some(attr.unescape_value()?.into_owned()));
        }
    }
    Ok(None)
}

fn opf_base_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[..idx + 1].to_string(),
        None => String::new(),
    }
}

fn resolve_href(base_dir: &str, href: &str) -> String {
    if href.contains("://") {
        return href.to_string();
    }
    if base_dir.is_empty() {
        return href.to_string();
    }
    let mut buf = PathBuf::from(base_dir);
    buf.push(href);
    buf.to_string_lossy().replace('\\', "/")
}

fn is_xml_name(name: &[u8], expected: &[u8]) -> bool {
    if name == expected {
        return true;
    }
    if let Some(idx) = name.iter().position(|b| *b == b':') {
        return &name[idx + 1..] == expected;
    }
    false
}

fn is_block_tag(name: &[u8]) -> bool {
    is_xml_name(name, b"p")
        || is_xml_name(name, b"div")
        || is_xml_name(name, b"li")
        || is_xml_name(name, b"blockquote")
        || is_xml_name(name, b"h1")
        || is_xml_name(name, b"h2")
        || is_xml_name(name, b"h3")
        || is_xml_name(name, b"h4")
        || is_xml_name(name, b"h5")
        || is_xml_name(name, b"h6")
}

fn heading_level_from(name: &[u8]) -> Option<u8> {
    if is_xml_name(name, b"h1") {
        Some(1)
    } else if is_xml_name(name, b"h2") {
        Some(2)
    } else if is_xml_name(name, b"h3") {
        Some(3)
    } else if is_xml_name(name, b"h4") {
        Some(4)
    } else if is_xml_name(name, b"h5") {
        Some(5)
    } else if is_xml_name(name, b"h6") {
        Some(6)
    } else {
        None
    }
}

fn is_pagebreak(e: &BytesStart<'_>) -> Result<bool, EpubError> {
    if let Some(value) = attr_value(e, b"epub:type")? {
        if value == "pagebreak" {
            return Ok(true);
        }
    }
    if let Some(value) = attr_value(e, b"role")? {
        if value == "doc-pagebreak" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn flush_text_run(
    runs: &mut Vec<TextRun>,
    current_text: &mut String,
    style: TextStyle,
    last_was_space: &mut bool,
) {
    if current_text.is_empty() {
        return;
    }
    if *last_was_space {
        // Avoid trailing spaces.
        while current_text.ends_with(' ') {
            current_text.pop();
        }
        *last_was_space = false;
    }
    if !current_text.is_empty() {
        runs.push(TextRun {
            text: current_text.clone(),
            style,
        });
        current_text.clear();
    }
}

fn flush_paragraph(
    blocks: &mut Vec<HtmlBlock>,
    runs: &mut Vec<TextRun>,
    current_text: &mut String,
    style: TextStyle,
    heading_level: Option<u8>,
) {
    if !current_text.is_empty() {
        runs.push(TextRun {
            text: current_text.clone(),
            style,
        });
        current_text.clear();
    }
    if runs.is_empty() {
        return;
    }
    let mut merged: Vec<TextRun> = Vec::new();
    for run in runs.drain(..) {
        if let Some(last) = merged.last_mut() {
            if last.style == run.style {
                last.text.push_str(&run.text);
                continue;
            }
        }
        merged.push(run);
    }
    blocks.push(HtmlBlock::Paragraph {
        runs: merged,
        heading_level,
    });
}

fn push_normalized_text(input: &str, buf: &mut String, last_was_space: &mut bool) {
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !*last_was_space {
                buf.push(' ');
                *last_was_space = true;
            }
        } else {
            buf.push(ch);
            *last_was_space = false;
        }
    }
}

fn build_spine_hrefs(package: &OpfPackage) -> Vec<String> {
    let mut manifest_map = HashMap::new();
    for item in &package.manifest {
        manifest_map.insert(item.id.as_str(), item.href.as_str());
    }
    let mut hrefs = Vec::new();
    for spine in &package.spine {
        if let Some(href) = manifest_map.get(spine.idref.as_str()) {
            hrefs.push(resolve_href(&package.opf_dir, href));
        }
    }
    hrefs
}

fn split_href_anchor(href: &str) -> (String, String) {
    if let Some(idx) = href.find('#') {
        (href[..idx].to_string(), href[idx + 1..].to_string())
    } else {
        (href.to_string(), String::new())
    }
}

fn flatten_toc(
    entries: &[TocEntry],
    level: u8,
    out: &mut Vec<CacheTocEntry>,
    spine_map: &HashMap<&str, i32>,
) {
    for entry in entries {
        let (path, anchor) = split_href_anchor(&entry.href);
        let spine_index = spine_map.get(path.as_str()).copied().unwrap_or(-1);
        out.push(CacheTocEntry {
            title: entry.label.clone(),
            href: path,
            anchor,
            level,
            spine_index,
        });
        if !entry.children.is_empty() {
            flatten_toc(&entry.children, level.saturating_add(1), out, spine_map);
        }
    }
}

fn zip_entry_size<R: Read + Seek>(archive: &mut zip::ZipArchive<R>, name: &str) -> Option<u64> {
    if let Ok(file) = archive.by_name(name) {
        return Some(file.size());
    }
    let name = name.strip_prefix("./").unwrap_or(name);
    archive.by_name(name).ok().map(|file| file.size())
}

fn system_time_secs(time: Option<SystemTime>) -> u64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_u8<R: Read>(reader: &mut R) -> Result<u8, EpubError> {
    let mut buf = [0u8; 1];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32, EpubError> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64, EpubError> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn read_i32<R: Read>(reader: &mut R) -> Result<i32, EpubError> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

fn read_string<R: Read>(reader: &mut R) -> Result<String, EpubError> {
    let len = read_u32(reader)? as usize;
    let mut buf = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut buf)?;
    }
    Ok(String::from_utf8(buf)?)
}

fn write_u8<W: Write>(writer: &mut W, value: u8) -> Result<(), EpubError> {
    writer.write_all(&[value])?;
    Ok(())
}

fn write_u32<W: Write>(writer: &mut W, value: u32) -> Result<(), EpubError> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn write_u64<W: Write>(writer: &mut W, value: u64) -> Result<(), EpubError> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn write_i32<W: Write>(writer: &mut W, value: i32) -> Result<(), EpubError> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn write_string<W: Write>(writer: &mut W, value: &str) -> Result<(), EpubError> {
    let bytes = value.as_bytes();
    write_u32(writer, bytes.len() as u32)?;
    writer.write_all(bytes)?;
    Ok(())
}
