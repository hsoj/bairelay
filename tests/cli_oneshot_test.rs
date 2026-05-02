//! Hardware-free subprocess tests for the one-shot CLI
//! subcommands. Each test drives the real `bairelay` binary via
//! `assert_cmd` and exercises a failure path that does not touch the
//! network or any camera.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn missing_config_file_exits_2() {
	// Missing config file is a usage mistake (operator pointed `-c` at
	// nothing), not a malformed config. Mirrors `check-config`'s
	// EXIT_USAGE mapping and the table in docs/architecture.md §
	// Error handling.
	Command::cargo_bin("bairelay")
		.unwrap()
		.args(["battery", "-c", "/nonexistent.toml", "cam"])
		.assert()
		.code(2);
}

#[test]
fn unknown_camera_name_exits_2() {
	// Unknown camera name is an operator typo (usage), not a malformed
	// config. EXIT_USAGE = 2 per docs/architecture.md § Error handling.
	let tmp = tempfile::NamedTempFile::new().unwrap();
	std::fs::write(
		tmp.path(),
		r#"
bind = "0.0.0.0"
[[cameras]]
name = "driveway"
username = "admin"
password = "x"
address = "192.168.1.1:9000"
"#,
	)
	.unwrap();
	Command::cargo_bin("bairelay")
		.unwrap()
		.args(["battery", "-c"])
		.arg(tmp.path())
		.arg("not-a-real-camera")
		.assert()
		.code(2)
		.stderr(predicate::str::contains("not-a-real-camera"));
}

#[test]
fn snapshot_json_without_output_exits_2() {
	// Pre-flight usage check must fire BEFORE the config read. Prove
	// this by pointing -c at a nonexistent path — if the ordering
	// regresses, we'd see exit 3 (config) instead of exit 2 (usage).
	Command::cargo_bin("bairelay")
		.unwrap()
		.args(["--json", "snapshot", "-c", "/nonexistent.toml", "driveway"])
		.assert()
		.code(2);
}

#[test]
fn help_lists_all_oneshot_subcommands() {
	Command::cargo_bin("bairelay")
		.unwrap()
		.arg("--help")
		.assert()
		.success()
		.stdout(
			predicate::str::contains("reboot")
				.and(predicate::str::contains("snapshot"))
				.and(predicate::str::contains("battery"))
				.and(predicate::str::contains("floodlight"))
				.and(predicate::str::contains("presets"))
				.and(predicate::str::contains("set-time"))
				.and(predicate::str::contains("version"))
				.and(predicate::str::contains("siren")),
		);
}
