use std::path::Path;

use clap::Parser;

use bairelay::cli::Cli;

#[test]
fn parse_mqtt_subcommand() {
	let cli = Cli::try_parse_from(["bairelay", "mqtt", "--config", "/etc/bairelay.toml"])
		.expect("should parse mqtt subcommand");
	assert_eq!(cli.config_path(), Path::new("/etc/bairelay.toml"));
	assert!(cli.wants_mqtt());
}

#[test]
fn parse_mqtt_rtsp_subcommand() {
	let cli = Cli::try_parse_from(["bairelay", "mqtt-rtsp", "-c", "custom.toml"])
		.expect("should parse mqtt-rtsp subcommand");
	assert_eq!(cli.config_path(), Path::new("custom.toml"));
	assert!(cli.wants_mqtt());
}

#[test]
fn parse_rtsp_subcommand() {
	let cli = Cli::try_parse_from(["bairelay", "rtsp", "--config", "/tmp/rtsp.toml"])
		.expect("should parse rtsp subcommand");
	assert_eq!(cli.config_path(), Path::new("/tmp/rtsp.toml"));
	assert!(!cli.wants_mqtt());
}

#[test]
fn require_subcommand() {
	let args = Cli::try_parse_from(["bairelay"]);
	assert!(args.is_err());
}

#[test]
fn default_config_path_is_config_toml() {
	// When no -c is specified, default should be "config.toml"
	let cli_mqtt =
		Cli::try_parse_from(["bairelay", "mqtt"]).expect("should parse mqtt without config flag");
	assert_eq!(cli_mqtt.config_path(), Path::new("config.toml"));

	let cli_rtsp =
		Cli::try_parse_from(["bairelay", "rtsp"]).expect("should parse rtsp without config flag");
	assert_eq!(cli_rtsp.config_path(), Path::new("config.toml"));

	let cli_both = Cli::try_parse_from(["bairelay", "mqtt-rtsp"])
		.expect("should parse mqtt-rtsp without config flag");
	assert_eq!(cli_both.config_path(), Path::new("config.toml"));
}

#[test]
fn config_path_for_each_subcommand() {
	let cases = vec![
		("mqtt", "/a.toml"),
		("rtsp", "/b.toml"),
		("mqtt-rtsp", "/c.toml"),
	];
	for (sub, path) in cases {
		let cli = Cli::try_parse_from(["bairelay", sub, "-c", path])
			.unwrap_or_else(|e| panic!("should parse {sub}: {e}"));
		assert_eq!(
			cli.config_path(),
			Path::new(path),
			"config_path() wrong for subcommand '{sub}'"
		);
	}
}

#[test]
fn wants_mqtt_returns_true_for_mqtt_commands() {
	let mqtt = Cli::try_parse_from(["bairelay", "mqtt"]).unwrap();
	assert!(mqtt.wants_mqtt(), "mqtt subcommand should want MQTT");

	let mqtt_rtsp = Cli::try_parse_from(["bairelay", "mqtt-rtsp"]).unwrap();
	assert!(
		mqtt_rtsp.wants_mqtt(),
		"mqtt-rtsp subcommand should want MQTT"
	);
}

#[test]
fn wants_mqtt_returns_false_for_rtsp_only() {
	let rtsp = Cli::try_parse_from(["bairelay", "rtsp"]).unwrap();
	assert!(!rtsp.wants_mqtt(), "rtsp subcommand should not want MQTT");
}

#[test]
fn dump_bcmedia_defaults_to_none() {
	for sub in ["mqtt", "rtsp", "mqtt-rtsp"] {
		let cli = Cli::try_parse_from(["bairelay", sub])
			.unwrap_or_else(|e| panic!("should parse {sub}: {e}"));
		assert!(
			cli.dump_bcmedia_path().is_none(),
			"{sub} must default --dump-bcmedia to None"
		);
	}
}

#[test]
fn dump_bcmedia_accepted_for_rtsp_and_mqtt_rtsp() {
	let rtsp = Cli::try_parse_from(["bairelay", "rtsp", "--dump-bcmedia", "/tmp/fx"])
		.expect("rtsp --dump-bcmedia should parse");
	assert_eq!(rtsp.dump_bcmedia_path(), Some(Path::new("/tmp/fx")));

	let mqtt_rtsp = Cli::try_parse_from(["bairelay", "mqtt-rtsp", "--dump-bcmedia", "/tmp/fx2"])
		.expect("mqtt-rtsp --dump-bcmedia should parse");
	assert_eq!(mqtt_rtsp.dump_bcmedia_path(), Some(Path::new("/tmp/fx2")));
}

#[test]
fn dump_bcmedia_rejected_for_mqtt_only_subcommand() {
	// The flag must NOT exist on `mqtt` (no video to capture).
	let result = Cli::try_parse_from(["bairelay", "mqtt", "--dump-bcmedia", "/tmp/fx"]);
	assert!(
		result.is_err(),
		"mqtt subcommand must reject --dump-bcmedia"
	);
}
