use std::env;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() < 2 {
        eprintln!("Usage: trusty-book <input.epub> <output.trbk> [--font path.ttf] [--sizes 8,10,12] [--font-bold path.ttf] [--font-italic path.ttf] [--font-bold-italic path.ttf] [--trimg-version 1|2]");
        std::process::exit(1);
    }

    let input = args.remove(0);
    let output = args.remove(0);

    let mut font = None;
    let mut font_bold = None;
    let mut font_italic = None;
    let mut font_bold_italic = None;
    let mut sizes = None;
    let mut trimg_version = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--font" => {
                i += 1;
                font = args.get(i).cloned();
            }
            "--font-bold" => {
                i += 1;
                font_bold = args.get(i).cloned();
            }
            "--font-italic" => {
                i += 1;
                font_italic = args.get(i).cloned();
            }
            "--font-bold-italic" => {
                i += 1;
                font_bold_italic = args.get(i).cloned();
            }
            "--sizes" => {
                i += 1;
                sizes = args.get(i).cloned();
            }
            "--trimg-version" => {
                i += 1;
                trimg_version = args.get(i).cloned();
            }
            _ => {}
        }
        i += 1;
    }

    let sizes = sizes
        .unwrap_or_else(|| "10".to_string())
        .split(',')
        .filter_map(|s| s.trim().parse::<u16>().ok())
        .collect::<Vec<_>>();

    let font_paths = trusty_book::FontPaths {
        regular: font,
        bold: font_bold,
        italic: font_italic,
        bold_italic: font_bold_italic,
    };

    let mut options = trusty_book::RenderOptions::default();
    if let Some(value) = trimg_version {
        options.trimg_version = match value.as_str() {
            "1" => 1,
            "2" => 2,
            _ => {
                eprintln!("Unsupported --trimg-version {value} (expected 1 or 2)");
                std::process::exit(1);
            }
        };
    }

    if let Err(err) = trusty_book::convert_epub_to_trbk_multi(
        &input,
        &output,
        &sizes,
        &font_paths,
        options.trimg_version,
    ) {
        eprintln!("Conversion failed: {err}");
        std::process::exit(1);
    }

    println!("Wrote TRBK output(s) starting at {output}");
}
