//! The camera's own network services — the ports it listens on and
//! whether each is switched on.
//!
//! Reolink firmware exposes six of these (Baichuan, HTTP, HTTPS, RTMP,
//! RTSP, ONVIF) through six structurally identical get/set command
//! pairs. Keying them off one enum is what lets the camera-facing code
//! carry a single pair of operations instead of twelve.

use std::fmt;

/// A camera network service whose port / enable state can be read or
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
	Baichuan,
	Http,
	Https,
	Rtmp,
	Rtsp,
	Onvif,
}

impl ServiceKind {
	pub const ALL: [ServiceKind; 6] = [
		ServiceKind::Baichuan,
		ServiceKind::Http,
		ServiceKind::Https,
		ServiceKind::Rtmp,
		ServiceKind::Rtsp,
		ServiceKind::Onvif,
	];

	pub fn label(self) -> &'static str {
		match self {
			ServiceKind::Baichuan => "baichuan",
			ServiceKind::Http => "http",
			ServiceKind::Https => "https",
			ServiceKind::Rtmp => "rtmp",
			ServiceKind::Rtsp => "rtsp",
			ServiceKind::Onvif => "onvif",
		}
	}
}

impl fmt::Display for ServiceKind {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.label())
	}
}

/// Current state of one camera network service.
///
/// `enabled` is `None` when the firmware omits the enable flag — some
/// models can report a service's port without being able to toggle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServicePortState {
	pub port: u32,
	pub enabled: Option<bool>,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn service_labels_are_stable() {
		// These strings reach operators through `bairelay services` and
		// through MQTT payloads, so they are contract, not cosmetics.
		let labels: Vec<_> = ServiceKind::ALL.iter().map(|s| s.label()).collect();
		assert_eq!(
			labels,
			vec!["baichuan", "http", "https", "rtmp", "rtsp", "onvif"]
		);
		assert_eq!(ServiceKind::Rtsp.to_string(), "rtsp");
	}
}
