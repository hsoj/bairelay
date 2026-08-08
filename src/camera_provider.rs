//! Adapter from the RTSP `StreamProvider` trait to the binary's
//! `CameraHandle` + `StreamSource`.
//!
//! Sits between [`rtsp`]'s server runtime (which only knows the
//! `StreamProvider` trait) and the per-camera task tree owned by the
//! orchestrator. Looks up the requested camera, enforces any per-camera
//! ACL, acquires a wake lock for the session lifetime, and hands back a
//! [`SubscriptionHandle`] populated with the live broadcast receiver,
//! current SDP parameters, and the shared last-frame buffer.

use crate::sync::RwLockPoisonRecover as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::rtsp::provider::{StreamError, StreamProvider, SubscriptionHandle};
use crate::rtsp::url::StreamKind;

use crate::camera::CameraHandle;
use crate::wake_lock::WakeLockGuard;

/// Deadline the provider waits for the first video SDP parameters before
/// giving up and returning `StreamError::Unavailable`. Must cover a cold
/// battery-camera wake followed by the first keyframe.
const SDP_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Return (should_await_audio, audio_bonus_window) based on the camera's
/// current `AudioPresence`. `wait_audio = true` means subscribe must wait
/// for `SdpParams.audio.is_some()` in addition to video before returning
/// a subscription to the RTSP server. `bonus_window` is zero unless the
/// state is `Unknown`, in which case the subscribe path grants a short
/// additional deadline after video is ready to observe audio before
/// committing to a video-only SDP response.
pub(crate) fn classify_presence(
	p: crate::audio_presence::AudioPresence,
) -> (bool, std::time::Duration) {
	use crate::audio_presence::AudioPresence;
	match p {
		AudioPresence::Present { .. } => (true, std::time::Duration::ZERO),
		AudioPresence::Absent => (false, std::time::Duration::ZERO),
		AudioPresence::Unknown => (false, std::time::Duration::from_secs(2)),
	}
}

/// Provider that adapts a camera registry into the RTSP `StreamProvider`
/// trait.
pub struct CameraProvider {
	cameras: Arc<HashMap<String, Arc<CameraHandle>>>,
}

impl CameraProvider {
	/// Construct from the orchestrator's camera registry (typically
	/// obtained via `Orchestrator::cameras_arc`).
	pub fn new(cameras: Arc<HashMap<String, Arc<CameraHandle>>>) -> Self {
		Self { cameras }
	}
}

#[async_trait::async_trait]
impl StreamProvider for CameraProvider {
	async fn subscribe(
		&self,
		camera: &str,
		kind: StreamKind,
		authenticated_user: Option<&str>,
	) -> Result<SubscriptionHandle, StreamError> {
		let handle = self.cameras.get(camera).ok_or(StreamError::UnknownCamera)?;

		// Per-camera access control. An empty allowlist means "any
		// authenticated user (or anonymous, when no auth is configured)
		// is allowed". `validate_config` guarantees every listed name
		// exists in the global `[[users]]` table.
		if let Some(user) = authenticated_user {
			let permitted = &handle.config().permitted_users;
			if !permitted.is_empty() && !permitted.iter().any(|u| u == user) {
				return Err(StreamError::AccessDenied);
			}
		}

		// Acquire a wake lock for the session. The guard rides inside
		// the returned `SubscriptionHandle` and is dropped by the RTSP
		// session task when the client disconnects, releasing the
		// reference and letting the grace-period watcher tear the
		// camera down if no other consumers remain.
		let guard: WakeLockGuard = handle.wake_lock().acquire();

		let source = handle.stream_source(kind).await?;

		// Branch on the camera's observed audio presence. On timeout we
		// surface Unavailable → 503, which is the appropriate user-visible
		// signal that the camera never produced usable bitstream. See
		// `classify_presence` for the policy.
		let presence = *handle.audio_presence().read_recover();
		let (wait_audio, bonus_window) = classify_presence(presence);

		let sdp_params = if wait_audio {
			// Present { codec }: wait for both video AND audio within
			// the same 15 s budget.
			source
				.await_sdp_both_ready(SDP_READY_TIMEOUT)
				.await
				.map_err(|e| StreamError::Unavailable(e.to_string()))?
		} else {
			// Absent or Unknown: wait for video only. The `?` drops the
			// returned snapshot — we re-read `sdp_params()` below so the
			// Unknown bonus window's result is reflected.
			source
				.await_sdp_ready(SDP_READY_TIMEOUT)
				.await
				.map_err(|e| StreamError::Unavailable(e.to_string()))?;
			if !bonus_window.is_zero() {
				// Unknown: give audio a bounded bonus window to show up.
				// Ignore the result — if audio didn't arrive, the final
				// snapshot stays video-only. A bonus-window timeout is
				// not a subscribe failure; the camera is already
				// streaming video.
				let _ = source.await_audio(bonus_window).await;
			}
			// Re-read: audio may have populated during the bonus window
			// (reader task's CAS upgraded presence to Present).
			source.sdp_params()
		};

		let frames = source.subscribe();
		let last_frame = source.last_frame();

		Ok(SubscriptionHandle {
			frames,
			sdp_params,
			last_frame,
			guard: Box::new(guard),
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Compile-time check that `CameraProvider` satisfies the
	/// `StreamProvider` trait bound (which itself implies `Send + Sync +
	/// 'static`).
	#[test]
	fn camera_provider_implements_stream_provider() {
		fn assert_impl<T: StreamProvider>() {}
		assert_impl::<CameraProvider>();
	}

	#[test]
	fn classify_present_waits_for_audio_no_bonus() {
		use crate::rtsp::codec::AudioCodec;
		let (wait, bonus) = classify_presence(crate::audio_presence::AudioPresence::Present {
			codec: AudioCodec::Aac,
		});
		assert!(wait);
		assert_eq!(bonus, std::time::Duration::ZERO);
	}

	#[test]
	fn classify_absent_no_wait_no_bonus() {
		let (wait, bonus) = classify_presence(crate::audio_presence::AudioPresence::Absent);
		assert!(!wait);
		assert_eq!(bonus, std::time::Duration::ZERO);
	}

	#[test]
	fn classify_unknown_bonus_window_two_seconds() {
		let (wait, bonus) = classify_presence(crate::audio_presence::AudioPresence::Unknown);
		assert!(!wait);
		assert_eq!(bonus, std::time::Duration::from_secs(2));
	}

	#[tokio::test]
	async fn camera_provider_subscribe_unknown_camera_rejects() {
		use crate::config::test_helpers::minimal_camera_config;
		use tokio_util::sync::CancellationToken;
		// Non-empty registry, but a subscribe for a different name must
		// return UnknownCamera without touching anything else.
		let cancel = CancellationToken::new();
		let handle = Arc::new(CameraHandle::new(
			minimal_camera_config("registered"),
			cancel,
			None,
		));
		let mut map = HashMap::new();
		map.insert("registered".to_string(), handle);
		let provider = CameraProvider::new(Arc::new(map));
		let err = provider
			.subscribe("nope", StreamKind::Main, None)
			.await
			.err()
			.expect("unknown camera");
		assert!(matches!(err, StreamError::UnknownCamera));
	}

	#[tokio::test]
	async fn camera_provider_subscribe_rejects_user_not_in_acl() {
		use crate::config::test_helpers::minimal_camera_config;
		use tokio_util::sync::CancellationToken;
		// Camera with a non-empty permitted_users list. A request by a
		// different authenticated user must return AccessDenied before
		// we ever touch the wake lock or the stream source.
		let cancel = CancellationToken::new();
		let mut cfg = minimal_camera_config("locked");
		cfg.permitted_users = vec!["alice".to_string()];
		let handle = Arc::new(CameraHandle::new(cfg, cancel, None));
		let mut map = HashMap::new();
		map.insert("locked".to_string(), handle);
		let provider = CameraProvider::new(Arc::new(map));
		let err = provider
			.subscribe("locked", StreamKind::Main, Some("bob"))
			.await
			.err()
			.expect("acl denies bob");
		assert!(matches!(err, StreamError::AccessDenied));
	}
}
