use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let icon_dir = Path::new("icons");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("icons.rs");

    let icons = [
        ("FOLDER", icon_dir.join("folder.svg")),
        ("GEAR", icon_dir.join("gear.svg")),
        ("BATTERY", icon_dir.join("battery.svg")),
    ];

    for (_, path) in &icons {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let mut output = String::new();
    output.push_str("pub const ICON_SIZE: u32 = 80;\n");

    for (name, path) in icons {
        let (dark, light) = render_svg_dual_mask(&path, 80, 80);
        output.push_str(&format!(
            "pub const ICON_{}_DARK_MASK: &[u8] = &[\n",
            name
        ));
        for chunk in dark.chunks(16) {
            output.push_str("    ");
            for byte in chunk {
                output.push_str(&format!("0x{:02X}, ", byte));
            }
            output.push('\n');
        }
        output.push_str("];\n\n");
        output.push_str(&format!(
            "pub const ICON_{}_LIGHT_MASK: &[u8] = &[\n",
            name
        ));
        for chunk in light.chunks(16) {
            output.push_str("    ");
            for byte in chunk {
                output.push_str(&format!("0x{:02X}, ", byte));
            }
            output.push('\n');
        }
        output.push_str("];\n\n");
    }

    fs::write(&out_path, output).unwrap();
}

fn render_svg_dual_mask(path: &Path, width: u32, height: u32) -> (Vec<u8>, Vec<u8>) {
    let data = fs::read(path).unwrap();
    let options = usvg::Options::default();
    let mut fontdb = usvg::fontdb::Database::new();
    fontdb.load_system_fonts();
    let tree = usvg::Tree::from_data(&data, &options, &fontdb).unwrap();
    let mut pixmap = tiny_skia::Pixmap::new(width, height).unwrap();
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap_mut);

    let mut dark = vec![0u8; ((width * height) as usize + 7) / 8];
    let mut light = vec![0u8; ((width * height) as usize + 7) / 8];
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) as usize;
            let byte = idx / 8;
            let bit = 7 - (idx % 8);
            let px = pixmap.pixel(x, y).unwrap();
            if px.alpha() > 0 {
                let r = px.red() as u16;
                let g = px.green() as u16;
                let b = px.blue() as u16;
                let lum = ((r * 30 + g * 59 + b * 11) / 100) as u8;
                if lum < 90 {
                    dark[byte] |= 1 << bit;
                } else if lum < 220 {
                    light[byte] |= 1 << bit;
                }
            }
        }
    }
    (dark, light)
}
