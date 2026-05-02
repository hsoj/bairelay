use std::env;
use std::fs;
use std::path::PathBuf;

// Bairelay version = "<major>.<minor>.<count>" where major/minor come
// from Cargo.toml and count is a monotonically increasing build number
// kept in `build-counter` at the package root (gitignored). Bumping
// the major or minor in Cargo.toml resets count to 0 on next build.
//
// The counter file lives next to Cargo.toml (NOT in target/) so it
// survives `cargo clean`. It's excluded from rerun-if-changed so
// writing to it does not trigger a rebuild loop.
fn main() {
	println!("cargo:rerun-if-changed=build.rs");
	println!("cargo:rerun-if-changed=Cargo.toml");
	println!("cargo:rerun-if-changed=src");
	println!("cargo:rerun-if-changed=crates");

	let major = env::var("CARGO_PKG_VERSION_MAJOR").expect("CARGO_PKG_VERSION_MAJOR");
	let minor = env::var("CARGO_PKG_VERSION_MINOR").expect("CARGO_PKG_VERSION_MINOR");
	let prefix = format!("{}.{}", major, minor);

	let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
	let counter_path = PathBuf::from(&manifest_dir).join("build-counter");

	let count: u64 = match fs::read_to_string(&counter_path) {
		Ok(content) => {
			let mut lines = content.lines();
			let stored_prefix = lines.next().unwrap_or("").trim();
			let stored_count: u64 = lines.next().unwrap_or("0").trim().parse().unwrap_or(0);
			if stored_prefix == prefix {
				stored_count.saturating_add(1)
			} else {
				0
			}
		}
		Err(_) => 0,
	};

	if let Err(e) = fs::write(&counter_path, format!("{}\n{}\n", prefix, count)) {
		println!(
			"cargo:warning=failed to persist build counter at {}: {}",
			counter_path.display(),
			e
		);
	}

	println!("cargo:rustc-env=BAIRELAY_VERSION={}.{}", prefix, count);
}
