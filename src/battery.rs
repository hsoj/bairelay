//! Battery charge and voltage as the camera reports them.
//!
//! Battery cameras are the whole reason bairelay exists, so their power
//! state gets first-class types rather than raw integers. The BC wire
//! value for voltage is an `i32` whose unit lived only in a doc comment;
//! [`Millivolts`] makes it a compiler fact.

use std::fmt;

/// Battery voltage in millivolts, as reported by the camera.
///
/// Carrying the unit in the type stops a caller from ever reading the
/// raw wire value as volts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Millivolts(pub i32);

impl Millivolts {
	/// Raw millivolt reading. Negative values (seen from some
	/// firmwares mid-boot) are preserved; clamp at the display edge.
	pub fn get(self) -> i32 {
		self.0
	}
}

impl fmt::Display for Millivolts {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{} mV", self.0)
	}
}

/// What a camera says about its battery: the subset of the BC
/// `BatteryInfo` XML that any caller actually reads, with units made
/// explicit. `percent` is clamped to 0–100 once, at the wire boundary,
/// so consumers don't each have to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatteryStatus {
	/// Charge percentage, clamped to 0–100.
	pub percent: u8,
	pub voltage: Millivolts,
	/// Firmware-reported charge status. Known values:
	/// `"chargeComplete"`, `"charging"`, `"none"`.
	pub charge_status: String,
	pub low_power: bool,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn millivolts_displays_with_unit() {
		assert_eq!(Millivolts(3985).to_string(), "3985 mV");
		assert_eq!(Millivolts(3985).get(), 3985);
	}

	#[test]
	fn millivolts_preserves_negative_readings() {
		// Some firmwares report a negative voltage mid-boot. The type
		// keeps it; clamping is a display concern.
		assert_eq!(Millivolts(-1).get(), -1);
	}
}
