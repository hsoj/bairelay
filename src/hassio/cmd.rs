//! Implementation of the `bairelay render-hassio-config` subcommand.

use std::path::Path;

use crate::config::validate_config;
use crate::hassio::{merge, options};

#[allow(clippy::too_many_arguments)]
pub fn run(
	options_json: &Path,
	overlay: Option<&Path>,
	mqtt_host: Option<String>,
	mqtt_port: Option<u16>,
	mqtt_user: Option<String>,
	mqtt_pass: Option<String>,
	mqtt_ssl: bool,
	output: Option<&Path>,
) -> anyhow::Result<()> {
	let opts_src = std::fs::read_to_string(options_json)
		.map_err(|e| anyhow::anyhow!("read options.json at {}: {e}", options_json.display()))?;
	let opts: options::HassioOptions =
		serde_json::from_str(&opts_src).map_err(|e| anyhow::anyhow!("parse options.json: {e}"))?;

	let mqtt_flags = options::MqttServiceFlags {
		host: mqtt_host.filter(|s| !s.is_empty()),
		port: mqtt_port,
		username: mqtt_user.filter(|s| !s.is_empty()),
		password: mqtt_pass.filter(|s| !s.is_empty()),
		ssl: mqtt_ssl,
	};

	let mut cfg = options::build_base_config(&opts, &mqtt_flags);

	if let Some(p) = overlay {
		if p.exists() {
			let src = std::fs::read_to_string(p)
				.map_err(|e| anyhow::anyhow!("read overlay at {}: {e}", p.display()))?;
			let overlay_cfg = merge::parse_overlay(&src).map_err(|e| anyhow::anyhow!("{e}"))?;
			cfg = merge::merge(cfg, overlay_cfg);
		}
	}

	validate_config(&cfg).map_err(|e| anyhow::anyhow!("merged config invalid: {e}"))?;

	let rendered =
		toml::to_string_pretty(&cfg).map_err(|e| anyhow::anyhow!("serialise config: {e}"))?;
	match output {
		Some(p) => std::fs::write(p, rendered)
			.map_err(|e| anyhow::anyhow!("write {}: {e}", p.display()))?,
		None => print!("{rendered}"),
	}
	Ok(())
}
