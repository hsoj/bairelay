//! Typed view of Supervisor's `/data/options.json`.

use serde::Deserialize;

use crate::config::{CameraConfig, Config, MqttServerConfig};

/// Flags supplied by the entrypoint shim from `bashio::services 'mqtt' '<field>'`.
/// Each field is `None` when Supervisor's MQTT integration isn't installed —
/// in that case the user's TOML overlay must carry the broker config.
#[derive(Debug, Clone, Default)]
pub struct MqttServiceFlags {
	pub host: Option<String>,
	pub port: Option<u16>,
	pub username: Option<String>,
	pub password: Option<String>,
	pub ssl: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HassioOptions {
	#[serde(default = "default_topic_prefix")]
	pub topic_prefix: String,
	#[serde(default = "default_log_level")]
	pub log_level: String,
	#[serde(default)]
	pub cameras: Vec<HassioCamera>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HassioCamera {
	pub name: String,
	pub host_or_uid: String,
	pub password: String,
}

fn default_topic_prefix() -> String {
	"bairelay".into()
}

fn default_log_level() -> String {
	"info".into()
}

/// Map the Supervisor-provided HA options and MQTT service flags onto a
/// bairelay [`Config`]. This is the minimal base; the operator's TOML
/// overlay (Task A6+) merges on top to fill in fields the HA options form
/// doesn't expose (CA cert path for MQTT TLS, per-camera username/uid,
/// discovery settings, etc.).
///
/// - `mqtt.host == None` leaves `cfg.mqtt = None` so the overlay can supply
///   it. Set both `username` and `password` together to populate
///   `credentials`; missing either leaves the broker unauthenticated.
/// - `mqtt.ssl` is intentionally ignored here — TLS to MQTT requires a CA
///   path that Supervisor's `bashio::services 'mqtt'` doesn't surface. The
///   field stays on [`MqttServiceFlags`] for forward-compat.
/// - Cameras default `username = "admin"` (Reolink's stock account); custom
///   accounts are an overlay concern.
/// - HA's single `host_or_uid` field always lands in `address`. UID-based
///   discovery (populating `uid` and adjusting `discovery`) is overlay-only.
pub fn build_base_config(opts: &HassioOptions, mqtt: &MqttServiceFlags) -> Config {
	let mut cfg = Config::default();

	if let Some(host) = &mqtt.host {
		let credentials = match (&mqtt.username, &mqtt.password) {
			(Some(u), Some(p)) => Some((u.clone(), p.clone())),
			_ => None,
		};
		cfg.mqtt = Some(MqttServerConfig {
			broker_addr: host.clone(),
			port: mqtt.port.unwrap_or(1883),
			credentials,
			ca: None,
			client_auth: None,
			topic_prefix: opts.topic_prefix.clone(),
			discovery: None,
		});
	}

	cfg.cameras = opts
		.cameras
		.iter()
		.map(|c| CameraConfig {
			name: c.name.clone(),
			address: Some(c.host_or_uid.clone()),
			uid: None,
			// Reolink's stock username on fresh cameras; overlay TOML
			// is the escape hatch for non-default accounts.
			username: "admin".to_string(),
			password: Some(c.password.clone()),
			..CameraConfig::default()
		})
		.collect();

	cfg
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_minimal_options_json() {
		let json = r#"{
			"topic_prefix": "bairelay",
			"log_level": "info",
			"cameras": [
				{"name": "Hallway", "host_or_uid": "ABC123", "password": "secret"}
			]
		}"#;
		let opts: HassioOptions = serde_json::from_str(json).unwrap();
		assert_eq!(opts.topic_prefix, "bairelay");
		assert_eq!(opts.log_level, "info");
		assert_eq!(opts.cameras.len(), 1);
		assert_eq!(opts.cameras[0].name, "Hallway");
		assert_eq!(opts.cameras[0].host_or_uid, "ABC123");
		assert_eq!(opts.cameras[0].password, "secret");
	}

	#[test]
	fn mqtt_service_flags_defaults_to_unset() {
		let flags = MqttServiceFlags::default();
		assert!(flags.host.is_none());
		assert!(flags.port.is_none());
		assert!(flags.username.is_none());
		assert!(flags.password.is_none());
		assert!(!flags.ssl);
	}

	#[test]
	fn builds_base_config_with_cameras_and_mqtt() {
		let opts = HassioOptions {
			topic_prefix: "bairelay".into(),
			log_level: "info".into(),
			cameras: vec![HassioCamera {
				name: "Hallway".into(),
				host_or_uid: "ABC123".into(),
				password: "secret".into(),
			}],
		};
		let mqtt = MqttServiceFlags {
			host: Some("core-mosquitto".into()),
			port: Some(1883),
			username: Some("addons".into()),
			password: Some("pw".into()),
			ssl: false,
		};
		let cfg = build_base_config(&opts, &mqtt);
		assert_eq!(cfg.cameras.len(), 1);
		assert_eq!(cfg.cameras[0].name, "Hallway");
		let m = cfg.mqtt.as_ref().expect("mqtt set");
		assert_eq!(m.broker_addr, "core-mosquitto");
		assert_eq!(m.port, 1883);
		assert_eq!(m.topic_prefix, "bairelay");
	}

	#[test]
	fn no_cameras_yields_empty_camera_list() {
		let opts = HassioOptions {
			topic_prefix: "bairelay".into(),
			log_level: "info".into(),
			cameras: vec![],
		};
		let cfg = build_base_config(&opts, &MqttServiceFlags::default());
		assert!(cfg.cameras.is_empty());
	}

	#[test]
	fn no_mqtt_injection_leaves_mqtt_unset() {
		let opts = HassioOptions {
			topic_prefix: "bairelay".into(),
			log_level: "info".into(),
			cameras: vec![],
		};
		let cfg = build_base_config(&opts, &MqttServiceFlags::default());
		assert!(
			cfg.mqtt.is_none(),
			"mqtt must stay None for overlay to fill in"
		);
	}

	#[test]
	fn ssl_flag_does_not_break_mqtt_propagation() {
		// Supervisor may report ssl=true (HA broker on 8883). The minimal
		// builder ignores it — TLS to MQTT requires a CA cert path the
		// overlay TOML must supply (`[mqtt] ca = "..."`). This test pins
		// that the base builder still produces a valid Config; the TOML
		// overlay layer (Task A6+) is where TLS materialises.
		let opts = HassioOptions {
			topic_prefix: "bairelay".into(),
			log_level: "info".into(),
			cameras: vec![],
		};
		let mqtt = MqttServiceFlags {
			host: Some("broker.example".into()),
			port: Some(8883),
			username: None,
			password: None,
			ssl: true,
		};
		let cfg = build_base_config(&opts, &mqtt);
		let m = cfg.mqtt.as_ref().expect("mqtt set");
		assert_eq!(m.broker_addr, "broker.example");
		assert_eq!(m.port, 8883);
		assert!(m.ca.is_none(), "TLS deferred to overlay");
	}
}
