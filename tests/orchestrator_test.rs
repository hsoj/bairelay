use tokio_util::sync::CancellationToken;

use bairelay::config::test_helpers::{minimal_camera_config, two_camera_config};
use bairelay::config::Config;

#[tokio::test]
async fn camera_state_starts_disconnected() {
	let config = minimal_camera_config("test_cam");
	let cancel = CancellationToken::new();
	let handle = bairelay::camera::CameraHandle::new(config, cancel, None);
	assert!(handle.state().is_disconnected());
}

#[tokio::test]
async fn cancellation_propagates_to_camera() {
	let config = minimal_camera_config("test_cam");
	let cancel = CancellationToken::new();
	let handle = bairelay::camera::CameraHandle::new(config, cancel.clone(), None);
	assert!(!handle.is_cancelled());
	cancel.cancel();
	assert!(handle.is_cancelled());
}

#[tokio::test]
async fn orchestrator_creates_cameras_from_config() {
	let config = two_camera_config();
	let cancel = CancellationToken::new();
	let orch = bairelay::orchestrator::Orchestrator::new(config, cancel, None);
	assert_eq!(orch.camera_count(), 2);
}

#[tokio::test]
async fn cameras_iterator_returns_all_cameras() {
	let config = two_camera_config();
	let cancel = CancellationToken::new();
	let orch = bairelay::orchestrator::Orchestrator::new(config, cancel, None);

	let names: Vec<&str> = orch.cameras().map(|c| c.name()).collect();
	assert_eq!(names.len(), 2);
	assert!(names.contains(&"cam1"));
	assert!(names.contains(&"cam2"));
}

#[tokio::test]
async fn cameras_arc_returns_shared_ref() {
	let config = two_camera_config();
	let cancel = CancellationToken::new();
	let orch = bairelay::orchestrator::Orchestrator::new(config, cancel, None);

	let arc = orch.cameras_arc();
	assert_eq!(arc.len(), 2);
	assert!(arc.contains_key("cam1"));
	assert!(arc.contains_key("cam2"));
}

#[tokio::test]
async fn cancel_token_returns_the_token() {
	let cancel = CancellationToken::new();
	let config = two_camera_config();
	let orch = bairelay::orchestrator::Orchestrator::new(config, cancel.clone(), None);

	// The orchestrator's cancel token should be the same one we passed in
	assert!(!orch.cancel_token().is_cancelled());
	cancel.cancel();
	assert!(orch.cancel_token().is_cancelled());
}

#[tokio::test]
async fn disabled_camera_is_excluded() {
	let mut cam1 = minimal_camera_config("enabled_cam");
	cam1.enabled = true;

	let mut cam2 = minimal_camera_config("disabled_cam");
	cam2.enabled = false;

	let config = Config {
		cameras: vec![cam1, cam2],
		..Default::default()
	};

	let cancel = CancellationToken::new();
	let orch = bairelay::orchestrator::Orchestrator::new(config, cancel, None);

	assert_eq!(orch.camera_count(), 1);
	assert!(orch.get_camera("enabled_cam").is_some());
	assert!(orch.get_camera("disabled_cam").is_none());
}

#[tokio::test]
async fn get_camera_returns_correct_handle() {
	let config = two_camera_config();
	let cancel = CancellationToken::new();
	let orch = bairelay::orchestrator::Orchestrator::new(config, cancel, None);

	let cam = orch.get_camera("cam1").expect("cam1 should exist");
	assert_eq!(cam.name(), "cam1");

	assert!(orch.get_camera("nonexistent").is_none());
}

#[tokio::test]
async fn orchestrator_run_exits_on_cancel() {
	let config = Config {
		cameras: vec![],
		..Default::default()
	};
	let cancel = CancellationToken::new();
	let orch = bairelay::orchestrator::Orchestrator::new(config, cancel.clone(), None);

	// With no cameras, run() should return immediately (empty JoinSet)
	let handle = tokio::spawn(async move {
		orch.run().await;
	});

	// Should complete quickly since there are no cameras to wait on
	let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
	assert!(
		result.is_ok(),
		"run() with no cameras should return promptly"
	);
}
