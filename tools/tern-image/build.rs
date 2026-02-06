use std::env;
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

fn main() {
    println!("cargo:rustc-env=TRUSTY_VERSION={}", git_tag());
    println!("cargo:rustc-env=TRUSTY_BUILD_TIME={}", build_time());
}
