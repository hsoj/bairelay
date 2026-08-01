//! Pan / tilt / zoom vocabulary.
//!
//! Only the PT-capable models (and the zoom-capable subset of those)
//! use any of this; fixed-mount cameras report no presets and refuse
//! the zoom command.

/// Absolute zoom position in the camera's own units.
///
/// The BC protocol takes the user-facing zoom factor ×1000 (factor
/// 1.0 → 1000). The one constructor performs that multiplication, so
/// call sites can never forget it or apply it twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZoomLevel(u32);

impl ZoomLevel {
	/// Build from the user-facing zoom factor (1.0 = no zoom).
	/// Negative factors clamp to zero.
	pub fn from_factor(factor: f32) -> Self {
		Self((factor.max(0.0) * 1000.0).round() as u32)
	}

	/// The raw ×1000 value the camera wire format expects.
	pub fn camera_units(self) -> u32 {
		self.0
	}
}

/// One PTZ preset slot as reported by the camera. Firmware reports
/// every slot; unassigned slots have no name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetSlot {
	pub id: u8,
	pub name: Option<String>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn zoom_level_multiplies_factor_by_1000() {
		assert_eq!(ZoomLevel::from_factor(1.0).camera_units(), 1000);
		assert_eq!(ZoomLevel::from_factor(2.5).camera_units(), 2500);
		assert_eq!(ZoomLevel::from_factor(0.0).camera_units(), 0);
	}

	#[test]
	fn zoom_level_clamps_negative_factor_to_zero() {
		assert_eq!(ZoomLevel::from_factor(-3.0).camera_units(), 0);
	}
}
