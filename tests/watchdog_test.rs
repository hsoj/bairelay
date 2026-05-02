use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use bairelay::camera::CameraHandle;
use bairelay::config::test_helpers::minimal_camera_config;
use bairelay::watchdog::Watchdog;

// Prune-grace state-machine coverage lives in `src/camera.rs::prune_grace_tests`
// (virtual-clock unit tests). Integration tests here focus on the sweep loop
// itself; they pass `Duration::ZERO` for grace because none of them exercise
// idle-source retention.

#[tokio::test]
async fn watchdog_runs_and_cancels() {
	let cancel = CancellationToken::new();
	let watchdog = Watchdog::new(Duration::from_millis(50), Duration::ZERO, cancel.clone());

	let cameras: Arc<HashMap<String, Arc<CameraHandle>>> = Arc::new(HashMap::new());

	let handle = tokio::spawn({
		let cameras = Arc::clone(&cameras);
		async move {
			watchdog.run(cameras).await;
		}
	});

	// Let the watchdog tick a couple of times, then cancel.
	tokio::time::sleep(Duration::from_millis(150)).await;
	cancel.cancel();

	// Should finish promptly after cancellation.
	tokio::time::timeout(Duration::from_secs(1), handle)
		.await
		.expect("watchdog should finish within 1s")
		.expect("watchdog task should not panic");
}

#[tokio::test]
async fn watchdog_does_not_panic_with_cameras() {
	let cancel = CancellationToken::new();
	let watchdog = Watchdog::new(Duration::from_millis(50), Duration::ZERO, cancel.clone());

	let config = minimal_camera_config("test_cam");
	let cam = Arc::new(CameraHandle::new(config, cancel.clone(), None));
	let mut cameras = HashMap::new();
	cameras.insert("test_cam".to_string(), cam);
	let cameras = Arc::new(cameras);

	let handle = tokio::spawn({
		let cameras = Arc::clone(&cameras);
		async move {
			watchdog.run(cameras).await;
		}
	});

	tokio::time::sleep(Duration::from_millis(150)).await;
	cancel.cancel();

	tokio::time::timeout(Duration::from_secs(1), handle)
		.await
		.expect("watchdog should finish within 1s (with cameras)")
		.expect("watchdog task should not panic (with cameras)");
}

#[tokio::test]
async fn watchdog_skips_camera_with_idle_disconnect_false() {
	// Camera with idle_disconnect=false should never be flagged by the
	// watchdog, regardless of connection or wake-lock state.
	let cancel = CancellationToken::new();
	let watchdog = Watchdog::new(Duration::from_millis(50), Duration::ZERO, cancel.clone());

	let config = minimal_camera_config("no_idle");
	assert!(!config.idle_disconnect);
	let cam = Arc::new(CameraHandle::new(config, cancel.clone(), None));
	assert!(cam.state().is_disconnected());

	let mut cameras = HashMap::new();
	cameras.insert("no_idle".to_string(), cam.clone());
	let cameras = Arc::new(cameras);

	let handle = tokio::spawn({
		let cameras = Arc::clone(&cameras);
		async move {
			watchdog.run(cameras).await;
		}
	});

	tokio::time::sleep(Duration::from_millis(200)).await;
	cancel.cancel();

	tokio::time::timeout(Duration::from_secs(1), handle)
		.await
		.expect("watchdog should finish (idle_disconnect=false)")
		.expect("watchdog should not panic (idle_disconnect=false)");

	// Camera should still be disconnected (watchdog did not touch it)
	assert!(cam.state().is_disconnected());
}

#[tokio::test]
async fn watchdog_skips_disconnected_camera_with_zero_wake_locks() {
	let cancel = CancellationToken::new();
	let watchdog = Watchdog::new(Duration::from_millis(50), Duration::ZERO, cancel.clone());

	let mut config = minimal_camera_config("idle_cam");
	config.idle_disconnect = true;

	let cam = Arc::new(CameraHandle::new(config, cancel.clone(), None));
	// Camera is Disconnected with 0 wake locks — watchdog should skip
	assert!(cam.state().is_disconnected());
	assert!(cam.wake_lock().is_idle());

	let mut cameras = HashMap::new();
	cameras.insert("idle_cam".to_string(), cam.clone());
	let cameras = Arc::new(cameras);

	let handle = tokio::spawn({
		let cameras = Arc::clone(&cameras);
		async move {
			watchdog.run(cameras).await;
		}
	});

	tokio::time::sleep(Duration::from_millis(200)).await;
	cancel.cancel();

	tokio::time::timeout(Duration::from_secs(1), handle)
		.await
		.expect("watchdog should finish (disconnected cam)")
		.expect("watchdog should not panic (disconnected cam)");

	// Still disconnected — watchdog never called request_disconnect
	// because the camera was not Connected
	assert!(cam.state().is_disconnected());
}

#[tokio::test]
async fn watchdog_does_not_disconnect_camera_with_active_wake_lock() {
	// A camera that has idle_disconnect=true but has wake locks held
	// should NOT be disconnected by the watchdog.
	let cancel = CancellationToken::new();
	let watchdog = Watchdog::new(Duration::from_millis(50), Duration::ZERO, cancel.clone());

	let mut config = minimal_camera_config("held_cam");
	config.idle_disconnect = true;
	let cam = Arc::new(CameraHandle::new(config, cancel.clone(), None));

	// Hold a wake lock
	let _guard = cam.wake_lock().acquire();
	assert!(!cam.wake_lock().is_idle());

	let mut cameras = HashMap::new();
	cameras.insert("held_cam".to_string(), cam.clone());
	let cameras = Arc::new(cameras);

	let handle = tokio::spawn({
		let cameras = Arc::clone(&cameras);
		async move {
			watchdog.run(cameras).await;
		}
	});

	tokio::time::sleep(Duration::from_millis(200)).await;
	cancel.cancel();

	tokio::time::timeout(Duration::from_secs(1), handle)
		.await
		.expect("watchdog should finish (active wake lock)")
		.expect("watchdog should not panic (active wake lock)");
}
