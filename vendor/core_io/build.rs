extern crate rustc_version;

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

struct Mapping(&'static str,&'static str);

fn parse_mappings(mut mappings: &'static str) -> Vec<Mapping> {
	// FIXME: The format used here used to be parsed directly by rustc, which
	// is why it's kind of weird. It should be changed to a saner format.

	const P1: &'static str = r#"-Mapping(""#;
	const P2: &'static str = r#"",""#;
	const P3: &'static str = "\")\n";

	trait TakePrefix: Sized {
		fn take_prefix(&mut self, mid: usize) -> Self;
	}

	impl<'a> TakePrefix for &'a str {
		fn take_prefix(&mut self, mid: usize) -> Self {
			let prefix = &self[..mid];
			*self = &self[mid..];
			prefix
		}
	}

	let mut result = Vec::with_capacity( mappings.len() / (P1.len()+40+P2.len()+40+P3.len()) );

	while mappings.len() != 0 {
		match (
			mappings.take_prefix(P1.len()),
			mappings.take_prefix(40),
			mappings.take_prefix(P2.len()),
			mappings.take_prefix(40),
			mappings.take_prefix(P3.len()),
		) {
			(P1, hash1, P2, hash2, P3) => result.push(Mapping(hash1, hash2)),
			_ => panic!("Invalid input in mappings"),
		}
	}

	result
}

type Cfg = Option<&'static str>;
type Date = &'static str;
/// A `ConditionalCfg` is basically a list of optional feature names
/// (`Cfg`s) separated by `Date`s. The dates specify ranges of compiler
/// versions for which to enable particular features.
type ConditionalCfg = (Cfg, &'static [(Date, Cfg)]);
const CONDITIONAL_CFGS: &'static [ConditionalCfg] = &[
	(None, &[("2019-02-24", Some("pattern_guards"))]),
	(None, &[("2018-08-14", Some("non_exhaustive"))]),
	(Some("unicode"), &[("2018-08-13", None)]),
	(None, &[("2018-01-01", Some("core_memchr"))]),
	(None, &[("2017-06-15", Some("no_collections"))]),
	(Some("rustc_unicode"), &[("2016-12-15", Some("std_unicode")), ("2017-03-03", None)]),
];

fn main() {
	let ver=rustc_version::version_meta();

	let io_commit=match env::var("CORE_IO_COMMIT") {
		Ok(c) => c,
		Err(env::VarError::NotUnicode(_)) => panic!("Invalid commit specified in CORE_IO_COMMIT"),
		Err(env::VarError::NotPresent) => {
			let mappings=parse_mappings(include_str!("mapping.rs"));

			let compiler=ver.commit_hash.expect("Couldn't determine compiler version");
			mappings
				.iter()
				.find(|&&Mapping(elem,_)|elem==compiler)
				.map(|m| m.1.to_owned())
				.unwrap_or_else(|| {
					// Fallback to latest known mapping for newer compilers.
					mappings.last().expect("No mappings available").1.to_owned()
				})
		}
	};

	for &(mut curcfg, rest) in CONDITIONAL_CFGS {
		for &(date, nextcfg) in rest {
			// if no commit_date is provided, assume compiler is current
			if ver.commit_date.as_ref().map_or(false,|d| &**d<date) {
				break;
			}
			curcfg = nextcfg;
		}
		if let Some(cfg) = curcfg {
			println!("cargo:rustc-cfg={}", cfg);
		}
	}

	let mut dest_path=PathBuf::from(env::var_os("OUT_DIR").unwrap());
	dest_path.push("io.rs");
	let mut f=File::create(&dest_path).unwrap();

	let mut target_path=PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
	target_path.push("src");
	target_path.push(io_commit);
	target_path.push("mod.rs");

	f.write_all(br#"#[path=""#).unwrap();
	f.write_all(target_path.into_os_string().into_string().unwrap().replace("\\", "\\\\").as_bytes()).unwrap();
	f.write_all(br#""] mod io;"#).unwrap();
}
