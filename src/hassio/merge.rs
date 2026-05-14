//! Deep-merge a HA-options-derived [`Config`] with a TOML overlay.

use crate::config::Config;

pub fn parse_overlay(toml_src: &str) -> Result<Config, String> {
	toml::from_str(toml_src).map_err(|e| format!("overlay parse error: {e}"))
}

/// Merge top-level fields from `overlay` into `base`. Overlay values
/// that differ from the type's default win; defaults are passthrough.
/// `cameras` is handled separately by [`merge_cameras`].
pub fn merge_top_level(mut base: Config, overlay: Config) -> Config {
	let defaults = Config::default();

	if overlay.bind_addr != defaults.bind_addr {
		base.bind_addr = overlay.bind_addr;
	}
	if overlay.bind_port != defaults.bind_port {
		base.bind_port = overlay.bind_port;
	}
	if overlay.certificate.is_some() {
		base.certificate = overlay.certificate;
	}
	if overlay.tls_bind_port.is_some() {
		base.tls_bind_port = overlay.tls_bind_port;
	}
	if overlay.tls_client_ca.is_some() {
		base.tls_client_ca = overlay.tls_client_ca;
	}
	if overlay.tls_client_auth != defaults.tls_client_auth {
		base.tls_client_auth = overlay.tls_client_auth;
	}
	if !overlay.users.is_empty() {
		base.users = overlay.users;
	}
	if overlay.mqtt.is_some() {
		base.mqtt = overlay.mqtt;
	}
	if overlay.wake_server.is_some() {
		base.wake_server = overlay.wake_server;
	}
	if overlay.push_listener.is_some() {
		base.push_listener = overlay.push_listener;
	}
	if overlay.stream_prune_grace_secs != defaults.stream_prune_grace_secs {
		base.stream_prune_grace_secs = overlay.stream_prune_grace_secs;
	}

	base
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

	#[test]
	fn overlay_overrides_top_level_fields() {
		let base = Config {
			bind_addr: "0.0.0.0".into(),
			bind_port: 8554,
			..Config::default()
		};
		let overlay_src = r#"
			bind = "127.0.0.1"
			stream_prune_grace_secs = 60
			cameras = []
		"#;
		let overlay = parse_overlay(overlay_src).unwrap();
		let merged = merge_top_level(base, overlay);
		assert_eq!(merged.bind_addr, "127.0.0.1", "overlay overrides bind_addr");
		assert_eq!(merged.bind_port, 8554, "base bind_port preserved");
		assert_eq!(merged.stream_prune_grace_secs, 60);
	}

	#[test]
	fn empty_overlay_preserves_base() {
		let base = Config {
			bind_addr: "127.0.0.1".into(),
			bind_port: 9000,
			stream_prune_grace_secs: 45,
			..Config::default()
		};
		let overlay = parse_overlay("cameras = []").expect("empty parses");
		let merged = merge_top_level(base, overlay);
		assert_eq!(merged.bind_addr, "127.0.0.1");
		assert_eq!(merged.bind_port, 9000);
		assert_eq!(merged.stream_prune_grace_secs, 45);
	}
}
