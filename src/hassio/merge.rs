//! Deep-merge a HA-options-derived [`Config`] with a TOML overlay.

use crate::config::Config;

pub fn parse_overlay(toml_src: &str) -> Result<Config, String> {
	toml::from_str(toml_src).map_err(|e| format!("overlay parse error: {e}"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_overlay_with_wake_server() {
		let src = r#"
			bind = "0.0.0.0"
			bind_port = 8554
			cameras = []

			[wake_server]
			enable = true
		"#;
		let cfg = parse_overlay(src).expect("parse ok");
		assert_eq!(cfg.bind_addr, "0.0.0.0");
		assert!(cfg.wake_server.is_some());
	}

	#[test]
	fn rejects_malformed_toml() {
		let src = "not = valid =";
		assert!(parse_overlay(src).is_err());
	}
}
