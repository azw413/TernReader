use std::env;
use std::path::Path;

use tern_image::{ConvertOptions, DitherMode, FitMode, RegionMode};

const BUILD_VERSION: &str = env!("TRUSTY_VERSION");
const BUILD_TIME: &str = env!("TRUSTY_BUILD_TIME");

fn usage() -> ! {
    eprintln!(
        "Usage:\n  tern-image convert <input> <output> [--size WxH] [--fit contain|cover|stretch|integer|width] [--dither bayer|none] [--region auto|none|crisp|barcode] [--trimg-version 1|2] [--yolo-model path] [--yolo-classes N] [--yolo-confidence F] [--yolo-nms F] [--invert] [--debug]\n\nDefaults: --size 480x800 --fit width --dither bayer --region auto --trimg-version 1"
    );
    std::process::exit(2);
}

fn parse_size(value: &str) -> Option<(u32, u32)> {
    let (w, h) = value.split_once('x')?;
    let w = w.parse().ok()?;
    let h = h.parse().ok()?;
    Some((w, h))
}

fn main() {
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_default();
    if cmd == "--version" || cmd == "-V" || cmd == "version" {
        println!("tern-image {BUILD_VERSION} ({BUILD_TIME})");
        return;
    }
    if cmd != "convert" {
        usage();
    }

    let input = args.next().unwrap_or_default();
    let output = args.next().unwrap_or_default();
    if input.is_empty() || output.is_empty() {
        usage();
    }

    let mut options = ConvertOptions::default();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--size" => {
                let value = args.next().unwrap_or_default();
                if let Some((w, h)) = parse_size(&value) {
                    options.width = w;
                    options.height = h;
                } else {
                    usage();
                }
            }
            "--fit" => {
                let value = args.next().unwrap_or_default();
                options.fit = match value.as_str() {
                    "contain" => FitMode::Contain,
                    "cover" => FitMode::Cover,
                    "stretch" => FitMode::Stretch,
                    "integer" => FitMode::Integer,
                    "width" => FitMode::Width,
                    _ => usage(),
                };
            }
            "--dither" => {
                let value = args.next().unwrap_or_default();
                options.dither = match value.as_str() {
                    "bayer" => DitherMode::Bayer,
                    "none" => DitherMode::None,
                    _ => usage(),
                };
            }
            "--region" => {
                let value = args.next().unwrap_or_default();
                options.region_mode = match value.as_str() {
                    "auto" => RegionMode::Auto,
                    "none" => RegionMode::None,
                    "crisp" => RegionMode::Crisp,
                    "barcode" => RegionMode::Barcode,
                    _ => usage(),
                };
            }
            "--trimg-version" => {
                let value = args.next().unwrap_or_default();
                options.trimg_version = match value.as_str() {
                    "1" => 1,
                    "2" => 2,
                    _ => usage(),
                };
            }
            "--yolo-model" => {
                let value = args.next().unwrap_or_default();
                if value.is_empty() {
                    usage();
                }
                options.yolo_model = Some(value.into());
            }
            "--yolo-classes" => {
                let value = args.next().unwrap_or_default();
                let parsed = value.parse().ok();
                if let Some(classes) = parsed {
                    options.yolo_num_classes = classes;
                } else {
                    usage();
                }
            }
            "--yolo-confidence" => {
                let value = args.next().unwrap_or_default();
                let parsed = value.parse().ok();
                if let Some(confidence) = parsed {
                    options.yolo_confidence = confidence;
                } else {
                    usage();
                }
            }
            "--yolo-nms" => {
                let value = args.next().unwrap_or_default();
                let parsed = value.parse().ok();
                if let Some(threshold) = parsed {
                    options.yolo_nms = threshold;
                } else {
                    usage();
                }
            }
            "--invert" => options.invert = true,
            "--debug" => options.debug = true,
            _ => usage(),
        }
    }

    let input_path = Path::new(&input);
    let output_path = Path::new(&output);
    let data = match std::fs::read(input_path) {
        Ok(data) => data,
        Err(err) => {
            eprintln!("Failed to read input: {err}");
            std::process::exit(1);
        }
    };

    let trimg = match tern_image::convert_bytes(&data, options) {
        Ok(trimg) => trimg,
        Err(err) => {
            eprintln!("Conversion failed: {err:?}");
            std::process::exit(1);
        }
    };

    if let Err(err) = tern_image::write_trimg(output_path, &trimg) {
        eprintln!("Failed to write output: {err}");
        std::process::exit(1);
    }
}
