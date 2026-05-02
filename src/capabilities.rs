//! Per-camera capability cache.
//!
//! Capabilities are discovered by calling
//! [`neolink_core::bc_protocol::BcCamera::get_support`] once per
//! successful connect and caching the relevant booleans on
//! [`crate::camera::CameraHandle`]. The HA MQTT discovery publisher
//! consults this cache to gate capability-dependent entities (PT
//! buttons on fixed-position cameras, etc.).
//!
//! Keep this struct minimal — add a field only when a live consumer
//! needs to gate on it.

/// Subset of the camera's reported `Support` XML relevant to HA
/// discovery. `Default` yields "no optional hardware" — the safe
/// assumption when the `get_support` call fails.
#[derive(Debug, Default, Clone, Copy)]
pub struct CameraCapabilities {
	/// `true` when the camera reports pan-tilt hardware via either
	/// `ptzMode` (non-empty string) or `ptzCfg` (non-zero integer).
	/// Drives the PT-button entity set in HA discovery.
	pub has_ptz: bool,
}

impl From<CameraCapabilities> for bairelay_mqtt::discovery::CameraCapabilitiesView {
	fn from(caps: CameraCapabilities) -> Self {
		Self {
			has_ptz: caps.has_ptz,
		}
	}
}

/// Decide whether a `Support.ptz_mode` / `Support.ptz_cfg` pair indicates
/// real motorised PTZ hardware. Reolink fixed-mount cameras (e.g. Altas)
/// report `ptzMode=none` in their support XML — non-empty but explicitly
/// NOT PTZ — which a naive `is_empty()` check misclassifies. Only modes
/// like `pt`, `ptz`, `3d` count as PTZ.
pub fn ptz_mode_indicates_ptz(ptz_mode: Option<&str>, ptz_cfg: Option<u32>) -> bool {
	let mode_active = ptz_mode
		.map(|s| {
			let t = s.trim().to_ascii_lowercase();
			!t.is_empty() && t != "none" && t != "0" && t != "false"
		})
		.unwrap_or(false);
	mode_active || ptz_cfg.map(|v| v != 0).unwrap_or(false)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ptz_mode_pt_is_ptz() {
		assert!(ptz_mode_indicates_ptz(Some("pt"), None));
		assert!(ptz_mode_indicates_ptz(Some("PTZ"), None));
		assert!(ptz_mode_indicates_ptz(Some(" 3d "), None));
	}

	#[test]
	fn ptz_mode_none_is_not_ptz() {
		assert!(!ptz_mode_indicates_ptz(Some("none"), None));
		assert!(!ptz_mode_indicates_ptz(Some("None"), None));
		assert!(!ptz_mode_indicates_ptz(Some(" NONE "), None));
		assert!(!ptz_mode_indicates_ptz(Some("0"), None));
		assert!(!ptz_mode_indicates_ptz(Some(""), None));
		assert!(!ptz_mode_indicates_ptz(None, None));
	}

	#[test]
	fn ptz_cfg_nonzero_is_ptz_even_when_mode_says_none() {
		assert!(ptz_mode_indicates_ptz(Some("none"), Some(1)));
	}

	#[test]
	fn ptz_cfg_zero_is_not_ptz() {
		assert!(!ptz_mode_indicates_ptz(None, Some(0)));
	}
}
