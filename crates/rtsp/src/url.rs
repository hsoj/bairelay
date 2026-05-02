//! URL path resolution for RTSP streams.
//!
//! Resolves paths such as `/cam1`, `/cam1/main`, `/cam1/MainStream` to
//! `(camera_name, StreamKind)` pairs. The **stream suffix** is
//! case-insensitive; the **camera name** is preserved verbatim and
//! returned as-is. Case-insensitive camera lookup, if desired, is the
//! caller's responsibility (the binary performs this during the
//! `StreamProvider::subscribe` path lookup).
//!
//! Percent-encoded paths are not decoded. Configured camera names are
//! constrained to `[A-Za-z0-9_-]` (see `src/config.rs` validation), so
//! percent-encoding cannot produce a name that would match a valid
//! camera entry.

use std::fmt;

/// Which video stream the RTSP client is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
	/// Primary high-definition stream.
	Main,
	/// Lower-resolution substream.
	Sub,
	/// Balanced stream; falls back to Sub if unsupported.
	Extern,
}

impl fmt::Display for StreamKind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			StreamKind::Main => f.write_str("main"),
			StreamKind::Sub => f.write_str("sub"),
			StreamKind::Extern => f.write_str("extern"),
		}
	}
}

/// A resolved RTSP URL path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
	/// Camera name as received in the URL, case preserved.
	pub camera: String,
	/// Resolved stream kind from the URL suffix.
	pub stream: StreamKind,
}

/// Resolve a URL path such as `/cam1`, `/cam1/main`, `/CAM1/MainStream` to
/// a `(camera_name, StreamKind)` pair. Case-insensitive on the stream
/// suffix; preserves case on the camera name.
///
/// Returns `None` for unparseable paths (wrong segment count, invalid
/// stream suffix).
pub fn resolve(path: &str) -> Option<ResolvedPath> {
	let trimmed = path.trim_start_matches('/').trim_end_matches('/');
	if trimmed.is_empty() {
		return None;
	}
	let parts: Vec<&str> = trimmed.split('/').collect();
	match parts.as_slice() {
		[cam] => Some(ResolvedPath {
			camera: (*cam).to_string(),
			stream: StreamKind::Main,
		}),
		[cam, suffix] => {
			let stream = parse_stream_suffix(suffix)?;
			Some(ResolvedPath {
				camera: (*cam).to_string(),
				stream,
			})
		}
		_ => None,
	}
}

fn parse_stream_suffix(s: &str) -> Option<StreamKind> {
	match s.to_ascii_lowercase().as_str() {
		"main" | "mainstream" => Some(StreamKind::Main),
		"sub" | "substream" => Some(StreamKind::Sub),
		"extern" | "externstream" => Some(StreamKind::Extern),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn resolves_bare_camera_to_main() {
		assert_eq!(
			resolve("/cam1"),
			Some(ResolvedPath {
				camera: "cam1".to_string(),
				stream: StreamKind::Main,
			})
		);
	}

	#[test]
	fn resolves_trailing_slash() {
		assert_eq!(
			resolve("/cam1/"),
			Some(ResolvedPath {
				camera: "cam1".to_string(),
				stream: StreamKind::Main,
			})
		);
	}

	#[test]
	fn resolves_all_stream_aliases() {
		for suffix in &["main", "mainstream", "MAIN", "MainStream"] {
			let r = resolve(&format!("/cam1/{suffix}")).unwrap();
			assert_eq!(r.stream, StreamKind::Main, "suffix {suffix}");
		}
		for suffix in &["sub", "substream", "SUB", "SubStream"] {
			let r = resolve(&format!("/cam1/{suffix}")).unwrap();
			assert_eq!(r.stream, StreamKind::Sub);
		}
		for suffix in &["extern", "externstream", "EXTERN"] {
			let r = resolve(&format!("/cam1/{suffix}")).unwrap();
			assert_eq!(r.stream, StreamKind::Extern);
		}
	}

	#[test]
	fn preserves_camera_name_case() {
		let r = resolve("/FrontDoor/sub").unwrap();
		assert_eq!(r.camera, "FrontDoor");
	}

	#[test]
	fn rejects_empty_and_root() {
		assert_eq!(resolve(""), None);
		assert_eq!(resolve("/"), None);
	}

	#[test]
	fn rejects_unknown_suffix() {
		assert_eq!(resolve("/cam1/unknown"), None);
	}

	#[test]
	fn rejects_extra_segments() {
		assert_eq!(resolve("/cam1/main/extra"), None);
	}

	#[test]
	fn stream_kind_display_matches_canonical_suffix() {
		assert_eq!(StreamKind::Main.to_string(), "main");
		assert_eq!(StreamKind::Sub.to_string(), "sub");
		assert_eq!(StreamKind::Extern.to_string(), "extern");
	}

	// Regression: any path that splits into 3+ non-empty segments must
	// be rejected. The traversal payloads below all violate the URL
	// shape `/{camera}` or `/{camera}/{stream}`. Pinning structural
	// rejection here defends a future change to `split('/')` or
	// `parse_stream_suffix` from quietly opening a traversal hole — a
	// camera name containing `..` would never validate at config
	// (alphanumeric + `_-` only), so a 2-segment match on `("..", x)`
	// can never resolve to a configured camera either.
	#[test]
	fn rejects_traversal_three_segments() {
		assert_eq!(resolve("/cam1/../etc"), None);
		assert_eq!(resolve("/../cam1/main"), None);
		assert_eq!(resolve("/cam1/main/extra"), None);
	}

	#[test]
	fn rejects_traversal_dotdot_as_stream_suffix() {
		// Two segments with `..` as the stream suffix → unknown suffix.
		assert_eq!(resolve("/cam1/.."), None);
		assert_eq!(resolve("/cam1/."), None);
	}

	#[test]
	fn dotdot_as_camera_name_returns_unmatched() {
		// `/../` collapses to just `..` after trim, which structurally
		// resolves as a 1-segment "camera name" of `..`. Configured
		// camera names are alphanumeric + `_-` only (config validation),
		// so no real camera ever matches this — the provider's lookup
		// returns UnknownCamera. The url parser stays purely structural.
		let r = resolve("/..").expect("structurally a 1-segment path");
		assert_eq!(r.camera, "..");
		assert_eq!(r.stream, StreamKind::Main);
	}
}
