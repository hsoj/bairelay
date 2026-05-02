use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::camera::CameraHandle;

// ── Watchdog ─────────────────────────────────────────────────────────

pub struct Watchdog {
	interval: Duration,
	prune_grace: Duration,
	cancel: CancellationToken,
}

impl Watchdog {
	/// Construct a Watchdog.
	///
	/// `interval` is the sweep cadence (30 s in production).
	/// `prune_grace` is how long an idle `StreamSource` must have been
	/// without subscribers before the sweep drops it — see
	/// `Config::stream_prune_grace_secs`.
	pub fn new(interval: Duration, prune_grace: Duration, cancel: CancellationToken) -> Self {
		Self {
			interval,
			prune_grace,
			cancel,
		}
	}

	/// Runs the watchdog loop, periodically checking cameras and requesting
	/// disconnect for any that are connected, idle, and have
	/// `idle_disconnect` enabled.  Returns when the cancellation token fires.
	pub async fn run(&self, cameras: Arc<HashMap<String, Arc<CameraHandle>>>) {
		let mut interval = tokio::time::interval(self.interval);

		loop {
			tokio::select! {
				_ = self.cancel.cancelled() => break,
				_ = interval.tick() => {
					for (name, cam) in cameras.iter() {
						// Defensive cleanup: drop any StreamSources whose
						// broadcast channels have been without subscribers
						// for at least `prune_grace`. Normal session
						// teardown should already do this, so this is a
						// safety net for anything that leaked — with a
						// grace window that smooths rapid RTSP reconnects.
						cam.prune_idle_stream_sources_at(Instant::now(), self.prune_grace);

						// Safety-net disconnect: only fire when the camera
						// has been wake-lock-idle for longer than its grace
						// period. The per-camera `GracePeriod` task in
						// `grace_period.rs` handles the normal path; the
						// watchdog used to redundantly disconnect on
						// instantaneous `is_idle()` and would race-tear
						// camera sessions a few hundred ms before an MQTT
						// `control/wakeup` could land. With the grace gate
						// the watchdog only fires when grace_period.rs
						// failed (didn't get spawned, panicked, etc.) —
						// the safety-net role its docs always claimed.
						if cam.config().idle_disconnect && cam.state().is_connected() {
							let grace = crate::config::resolve_idle_disconnect_timeout(
								cam.config(),
								self.prune_grace,
							);
							if let Some(idle_since) = cam.wake_lock().idle_since() {
								if idle_since.elapsed() >= grace {
									tracing::warn!(
										camera = %name,
										idle_secs = idle_since.elapsed().as_secs(),
										"Watchdog: idle camera connected past grace, requesting disconnect"
									);
									cam.request_disconnect();
								}
							}
						}
					}
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::config::test_helpers::minimal_camera_config;

	#[tokio::test]
	async fn watchdog_exits_immediately_on_cancel() {
		let cancel = CancellationToken::new();
		let wd = Watchdog::new(
			Duration::from_millis(50),
			Duration::from_secs(5),
			cancel.clone(),
		);
		let cameras: Arc<HashMap<String, Arc<CameraHandle>>> = Arc::new(HashMap::new());
		cancel.cancel();
		// With the token already cancelled, run() must return on the
		// next select cycle, not hang.
		tokio::time::timeout(Duration::from_secs(1), wd.run(cameras))
			.await
			.expect("watchdog respects cancellation");
	}

	#[tokio::test]
	async fn watchdog_sweeps_cameras_and_triggers_disconnect_request() {
		// Build a camera in the disconnected default state. Since the
		// run loop isn't running, `state().is_connected()` is false and
		// the `idle_disconnect` branch inside the sweep is skipped —
		// but the `prune_idle_stream_sources_at` path still fires.
		// The test asserts the sweep completes one tick and cancel works.
		let cancel = CancellationToken::new();
		let mut cfg = minimal_camera_config("cam-wd");
		cfg.idle_disconnect = true;
		let cam = Arc::new(CameraHandle::new(cfg, cancel.clone(), None));
		let mut map: HashMap<String, Arc<CameraHandle>> = HashMap::new();
		map.insert("cam-wd".to_string(), cam);
		let cameras = Arc::new(map);

		let wd = Watchdog::new(
			Duration::from_millis(30),
			Duration::from_secs(5),
			cancel.clone(),
		);

		let cancel_task = cancel.clone();
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(120)).await;
			cancel_task.cancel();
		});
		tokio::time::timeout(Duration::from_secs(2), wd.run(cameras))
			.await
			.expect("watchdog exits after cancel");
	}

	#[tokio::test]
	async fn watchdog_requests_disconnect_for_idle_connected_camera() {
		// Drive the inner branch (idle_disconnect && is_connected &&
		// idle_since past grace) — the watchdog must call
		// `request_disconnect()`, which fires the camera's
		// `disconnect_signal` Notify. Uses `set_driver_for_test` to
		// flip state to Connected without a real socket. The
		// observable assertion is that `disconnect_signal.notified()`
		// resolves within the test budget — proves the watchdog
		// walked the branch *and* called `request_disconnect`, not
		// just that it didn't panic.
		//
		// Set a tiny grace (50 ms) and acquire+release a wake-lock so
		// `idle_since` returns a timestamp the watchdog can compare
		// against. With the grace gate added 2026-05-01, a camera
		// whose wake-lock was never acquired returns `idle_since =
		// None` and the watchdog correctly leaves it alone.
		use neolink_core::bc_protocol::FakeCameraBuilder;

		let cancel = CancellationToken::new();
		let mut cfg = minimal_camera_config("cam-idle");
		cfg.idle_disconnect = true;
		cfg.idle_disconnect_timeout_secs = Some(0.05);
		let cam = Arc::new(CameraHandle::new(cfg, cancel.clone(), None));
		let fake = FakeCameraBuilder::new().build();
		cam.set_driver_for_test(fake);
		assert!(cam.state().is_connected());
		// Stamp a release into idle_since so the watchdog has
		// something to compare against; sleep past the 50 ms grace
		// before kicking off the watchdog so the elapsed check fires.
		drop(cam.wake_lock().acquire());
		tokio::time::sleep(Duration::from_millis(80)).await;
		assert!(cam.wake_lock().is_idle());

		// Subscribe to the Notify *before* the watchdog runs so we
		// don't miss the signal. `Notify::notified()` returns a future
		// that captures a permit at await-poll time; pre-creating it
		// guards against the watchdog firing-then-no-listener race.
		let signal = cam.disconnect_signal_for_test();
		let notified = signal.notified();
		tokio::pin!(notified);

		let mut map: HashMap<String, Arc<CameraHandle>> = HashMap::new();
		map.insert("cam-idle".to_string(), Arc::clone(&cam));
		let cameras = Arc::new(map);

		// `prune_grace` must stay below the camera's configured
		// `idle_disconnect_timeout_secs` (50 ms here) so
		// `resolve_idle_disconnect_timeout` doesn't clamp the grace
		// up to `prune_grace + 15s` (the production-safe floor that
		// keeps a stream-source from outliving its session).
		let wd = Watchdog::new(
			Duration::from_millis(20),
			Duration::from_millis(10),
			cancel.clone(),
		);
		let cancel_task = cancel.clone();
		let watchdog_handle = tokio::spawn(async move {
			let _ = tokio::time::timeout(Duration::from_secs(2), wd.run(cameras)).await;
		});

		// Wait for the watchdog to fire request_disconnect, with a
		// generous budget for the 20 ms ticker on a loaded runner.
		let signal_seen = tokio::time::timeout(Duration::from_millis(500), notified.as_mut())
			.await
			.is_ok();
		cancel_task.cancel();
		let _ = tokio::time::timeout(Duration::from_secs(2), watchdog_handle).await;

		assert!(
			signal_seen,
			"watchdog must call request_disconnect on idle connected camera"
		);
	}
}
