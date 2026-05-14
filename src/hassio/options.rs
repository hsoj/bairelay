//! Typed view of Supervisor's `/data/options.json`.

use serde::Deserialize;

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
}
