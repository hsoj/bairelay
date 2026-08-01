use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use bairelay::camera::CameraHandle;
use bairelay::config::test_helpers::minimal_camera_config;
use bairelay::mqtt::control::parse_control_message;
use bairelay::mqtt_dispatch::dispatch_control;

/// Default MQTT topic prefix used in these dispatch tests — matches
/// the bairelay config default.
const PREFIX: &str = "bairelay";

/// Create a SharedMqttClient backed by a dummy (unconnected) broker.
/// The dispatch tests that exercise early-return paths never actually
/// publish, so the client is never used — but the signature requires one.
fn dummy_mqtt_client() -> bairelay::mqtt::SharedMqttClient {
	let cfg = bairelay::mqtt::MqttConfig {
		broker_addr: "127.0.0.1".to_string(),
		port: 0,
		credentials: None,
		ca: None,
		client_auth: None,
	};
	let (client, _event_loop) = bairelay::mqtt::connect(&cfg, "test-dispatch", PREFIX).unwrap();
	client
}

// ── dispatch_control: unknown camera ─────────────────────────────────

#[tokio::test]
async fn dispatch_unknown_camera_does_not_panic() {
	let cameras: HashMap<String, Arc<CameraHandle>> = HashMap::new();
	let mqtt = dummy_mqtt_client();

	let cmd = parse_control_message(PREFIX, "bairelay/nonexistent/control/floodlight", b"on")
		.expect("valid control message");

	// Should return without panic — the camera is not in the map.
	dispatch_control(cmd, &cameras, &mqtt, PREFIX).await;
}

// ── dispatch_control: disconnected camera ────────────────────────────

#[tokio::test]
async fn dispatch_disconnected_camera_does_not_panic() {
	let cancel = CancellationToken::new();
	let config = minimal_camera_config("garden");
	let cam = Arc::new(CameraHandle::new(config, cancel.clone(), None));

	let mut cameras: HashMap<String, Arc<CameraHandle>> = HashMap::new();
	cameras.insert("garden".to_string(), cam);

	let mqtt = dummy_mqtt_client();

	let cmd = parse_control_message(PREFIX, "bairelay/garden/control/floodlight", b"on")
		.expect("valid control message");

	// Camera exists but bc_camera() is None — dispatch now waits for
	// connect (15 s production budget) before bailing. Cancel first
	// so the wait short-circuits and the test stays fast.
	cancel.cancel();
	dispatch_control(cmd, &cameras, &mqtt, PREFIX).await;
}

// ── dispatch_control: wake lock acquired for disconnected camera ───

#[tokio::test]
async fn dispatch_to_disconnected_camera_wakes_then_bails_on_cancel() {
	// New behaviour (post issue #1): non-Wakeup control commands
	// still acquire the wake lock when the camera is disconnected
	// (so the run loop sees the acquire edge and starts connecting).
	// With no run loop in this test, dispatch waits for connect (15 s
	// budget) and bails on the camera's cancel token. The wake-lock
	// guard is released as the dispatch returns, leaving the lock
	// idle again.
	let cancel = CancellationToken::new();
	let config = minimal_camera_config("porch");
	let cam = Arc::new(CameraHandle::new(config, cancel.clone(), None));

	assert!(cam.wake_lock().is_idle(), "wake lock should start idle");

	let mut cameras: HashMap<String, Arc<CameraHandle>> = HashMap::new();
	cameras.insert("porch".to_string(), Arc::clone(&cam));

	let mqtt = dummy_mqtt_client();

	let cmd = parse_control_message(PREFIX, "bairelay/porch/control/reboot", b"")
		.expect("valid control message");

	cancel.cancel();
	dispatch_control(cmd, &cameras, &mqtt, PREFIX).await;

	assert!(
		cam.wake_lock().is_idle(),
		"wake lock must be released back to idle once dispatch returns"
	);
}

// ── dispatch_control: wake lock not leaked for unknown camera ────────

#[tokio::test]
async fn dispatch_to_unknown_does_not_leak_wake_lock() {
	let cameras: HashMap<String, Arc<CameraHandle>> = HashMap::new();
	let mqtt = dummy_mqtt_client();

	let cmd = parse_control_message(PREFIX, "bairelay/ghost/control/siren", b"on")
		.expect("valid control message");

	// No camera in map — nothing to leak, just verify no panic.
	dispatch_control(cmd, &cameras, &mqtt, PREFIX).await;
}

// ── dispatch_control: multiple commands to disconnected camera ───────

#[tokio::test]
async fn dispatch_multiple_commands_to_disconnected() {
	let cancel = CancellationToken::new();
	let config = minimal_camera_config("multi");
	let cam = Arc::new(CameraHandle::new(config, cancel.clone(), None));

	let mut cameras: HashMap<String, Arc<CameraHandle>> = HashMap::new();
	cameras.insert("multi".to_string(), Arc::clone(&cam));

	let mqtt = dummy_mqtt_client();

	let topics_and_payloads = [
		("bairelay/multi/control/floodlight", b"on" as &[u8]),
		("bairelay/multi/control/led", b"off"),
		("bairelay/multi/control/ir", b"auto"),
		("bairelay/multi/control/pir", b"on"),
		("bairelay/multi/control/reboot", b""),
		("bairelay/multi/control/ptz", b"up 10"),
		("bairelay/multi/control/zoom", b"0.5"),
		("bairelay/multi/control/siren", b"on"),
		("bairelay/multi/control/wakeup", b"5"),
		("bairelay/multi/query/battery", b""),
		("bairelay/multi/query/pir", b""),
	];

	// Cancel up front so each disconnected-dispatch short-circuits
	// instead of waiting the full 15 s connect budget per command.
	cancel.cancel();
	for (topic, payload) in &topics_and_payloads {
		let cmd = parse_control_message(PREFIX, topic, payload)
			.unwrap_or_else(|| panic!("valid control message for {topic}"));
		dispatch_control(cmd, &cameras, &mqtt, PREFIX).await;
	}

	// Wake lock should be idle once the last dispatch returns — each
	// acquire's RAII guard releases as the call unwinds.
	assert!(
		cam.wake_lock().is_idle(),
		"wake lock must return to idle after dispatch unwinds"
	);
}
