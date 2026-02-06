use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn git_tag() -> String {
    if let Ok(tag) = env::var("TRUSTY_VERSION") {
        if !tag.trim().is_empty() {
            return tag;
        }
    }
    let output = Command::new("git")
        .args(["describe", "--tags", "--dirty", "--always"])
        .output();
    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    }
}

fn build_time() -> String {
    use time::format_description::parse;
    use time::OffsetDateTime;

    let format = parse("[year]-[month]-[day] [hour]:[minute]").unwrap();
    OffsetDateTime::now_utc().format(&format).unwrap_or_else(|_| "unknown".to_string())
}

fn pack_mask(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; (bits.len() + 7) / 8];
    for (i, &bit) in bits.iter().enumerate() {
        if bit {
            let byte = i / 8;
            let shift = 7 - (i % 8);
            out[byte] |= 1 << shift;
        }
    }
    out
}

fn render_icon(svg_path: &Path, size: u32) -> (Vec<u8>, Vec<u8>) {
    render_icon_fit(svg_path, size, size)
}

fn render_icon_fit(svg_path: &Path, target_w: u32, target_h: u32) -> (Vec<u8>, Vec<u8>) {
    let data = fs::read(svg_path).expect("read svg");
    let opt = usvg::Options::default();
    let mut fontdb = usvg::fontdb::Database::new();
    fontdb.load_system_fonts();
    let tree = usvg::Tree::from_data(&data, &opt, &fontdb).expect("parse svg");

    let mut pixmap = tiny_skia::Pixmap::new(target_w, target_h).expect("pixmap");
    let tree_size = tree.size();
    let scale_x = target_w as f32 / tree_size.width();
    let scale_y = target_h as f32 / tree_size.height();
    let scale = scale_x.min(scale_y);
    let render_w = tree_size.width() * scale;
    let render_h = tree_size.height() * scale;
    let tx = (target_w as f32 - render_w) * 0.5;
    let ty = (target_h as f32 - render_h) * 0.5;
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty);
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, transform, &mut pixmap_mut);

    let mut dark_bits = vec![false; (target_w * target_h) as usize];
    let mut light_bits = vec![false; (target_w * target_h) as usize];
    let mut idx = 0usize;
    for y in 0..target_h {
        for x in 0..target_w {
            let p = pixmap.pixel(x, y).unwrap();
            let a = p.alpha();
            if a == 0 {
                idx += 1;
                continue;
            }
            let r = p.red() as u32;
            let g = p.green() as u32;
            let b = p.blue() as u32;
            let luma = (r * 2126 + g * 7152 + b * 722) / 10000;
            if luma < 110 {
                dark_bits[idx] = true;
            } else if luma < 235 {
                light_bits[idx] = true;
            }
            idx += 1;
        }
    }
    (pack_mask(&dark_bits), pack_mask(&light_bits))
}

fn write_icons(out_dir: &Path) {
    let icon_dir = Path::new("icons");
    let size = 80u32;
    let logo_path = Path::new("../ternreader_logo_4color.svg");
    let logo_w = 600u32;
    let logo_h = 180u32;

    let (folder_dark, folder_light) = render_icon(&icon_dir.join("folder.svg"), size);
    let (gear_dark, gear_light) = render_icon(&icon_dir.join("gear.svg"), size);
    let (battery_dark, battery_light) = render_icon(&icon_dir.join("battery.svg"), size);
    let (logo_dark, logo_light) = render_icon_fit(logo_path, logo_w, logo_h);

    let mut output = String::new();
    output.push_str(&format!("pub const ICON_SIZE: usize = {size};\n"));
    output.push_str(&format!("pub const LOGO_WIDTH: usize = {logo_w};\n"));
    output.push_str(&format!("pub const LOGO_HEIGHT: usize = {logo_h};\n"));

    let emit = |out: &mut String, name: &str, data: &[u8]| {
        out.push_str(&format!("pub const {name}: &[u8] = &[\n"));
        for chunk in data.chunks(16) {
            out.push_str("    ");
            for byte in chunk {
                out.push_str(&format!("0x{byte:02X}, "));
            }
            out.push_str("\n");
        }
        out.push_str("];\n");
    };

    emit(&mut output, "ICON_FOLDER_DARK_MASK", &folder_dark);
    emit(&mut output, "ICON_FOLDER_LIGHT_MASK", &folder_light);
    emit(&mut output, "ICON_GEAR_DARK_MASK", &gear_dark);
    emit(&mut output, "ICON_GEAR_LIGHT_MASK", &gear_light);
    emit(&mut output, "ICON_BATTERY_DARK_MASK", &battery_dark);
    emit(&mut output, "ICON_BATTERY_LIGHT_MASK", &battery_light);
    emit(&mut output, "LOGO_DARK_MASK", &logo_dark);
    emit(&mut output, "LOGO_LIGHT_MASK", &logo_light);

    fs::write(out_dir.join("icons.rs"), output).expect("write icons.rs");
}

fn main() {
    let out_dir = env::var_os("OUT_DIR").unwrap();
    write_icons(Path::new(&out_dir));
    println!("cargo:rustc-env=TRUSTY_VERSION={}", git_tag());
    println!("cargo:rustc-env=TRUSTY_BUILD_TIME={}", build_time());
}
