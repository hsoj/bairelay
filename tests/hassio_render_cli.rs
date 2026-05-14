//! End-to-end shell test for `bairelay render-hassio-config`. Drives
//! the real binary against a synthetic options.json + overlay.toml.

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::TempDir;

#[test]
fn render_hassio_config_produces_valid_config() {
	let tmp = TempDir::new().unwrap();
	let opts = tmp.path().join("options.json");
	let overlay = tmp.path().join("overlay.toml");
	let out = tmp.path().join("merged.toml");

	std::fs::write(
		&opts,
		r#"{
			"topic_prefix": "bairelay",
			"log_level": "info",
			"cameras": [{"name": "Hallway", "host_or_uid": "ABC123", "password": "secret"}]
		}"#,
	)
	.unwrap();
	std::fs::write(
		&overlay,
		r#"
[wake_server]
enable = true

[[cameras]]
name = "Hallway"
username = "admin"
channel_id = 0
"#,
	)
	.unwrap();

	Command::cargo_bin("bairelay")
		.unwrap()
		.args([
			"render-hassio-config",
			"--options-json",
			opts.to_str().unwrap(),
			"--overlay",
			overlay.to_str().unwrap(),
			"--mqtt-host",
			"core-mosquitto",
			"--mqtt-port",
			"1883",
			"--mqtt-user",
			"addons",
			"--mqtt-pass",
			"pw",
			"--output",
			out.to_str().unwrap(),
		])
		.assert()
		.success();

	let rendered = std::fs::read_to_string(&out).unwrap();
	assert!(rendered.contains("core-mosquitto"));
	assert!(rendered.contains("Hallway"));
	assert!(rendered.contains("[wake_server]"));

	// Round-trip via check-config to validate it's a real bairelay TOML.
	Command::cargo_bin("bairelay")
		.unwrap()
		.args(["check-config", "-c", out.to_str().unwrap()])
		.assert()
		.success();
}

#[test]
fn render_hassio_config_fails_on_missing_options_file() {
	Command::cargo_bin("bairelay")
		.unwrap()
		.args([
			"render-hassio-config",
			"--options-json",
			"/tmp/nonexistent-options.json",
		])
		.assert()
		.failure()
		.stderr(contains("read options.json"));
}
