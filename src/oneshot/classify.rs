use super::errors::{ConfigError, InterruptedError, UsageError};
use crate::baichuan::bc_protocol::Error as CoreError;

pub const EXIT_OK: i32 = 0;
pub const EXIT_UNEXPECTED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_CONFIG: i32 = 3;
pub const EXIT_CONNECTION: i32 = 4;
pub const EXIT_PROTOCOL: i32 = 5;
/// Camera protocol is fine but this model / user lacks the capability
/// being requested (e.g. `status-light` on an Argus battery cam).
pub const EXIT_UNSUPPORTED: i32 = 6;
pub const EXIT_INTERRUPTED: i32 = 130;

pub fn classify(err: &anyhow::Error) -> i32 {
	for cause in err.chain() {
		if cause.is::<InterruptedError>() {
			return EXIT_INTERRUPTED;
		}
		if cause.is::<UsageError>() {
			return EXIT_USAGE;
		}
		if cause.is::<ConfigError>() {
			return EXIT_CONFIG;
		}
		if let Some(core_err) = cause.downcast_ref::<CoreError>() {
			return classify_core(core_err);
		}
	}
	EXIT_UNEXPECTED
}

fn classify_core(err: &CoreError) -> i32 {
	match err {
		// Auth / login family — camera rejected our credentials, or
		// login handshake returned an error status.
		CoreError::CameraLoginFail
		| CoreError::AuthFailed
		// Discovery family — no reply, register refused us, relay
		// lookup failed, no dmap / no dev from reolink servers.
		| CoreError::DiscoveryTimeout
		| CoreError::DiscoveryIgnored
		| CoreError::NoDmap
		| CoreError::NoDev
		| CoreError::RegisterError
		// Transport drop family — TCP / UDP / relay / camera
		// terminated the connection partway through.
		| CoreError::DroppedConnection
		| CoreError::DroppedConnectionTry(_)
		| CoreError::BroadcastDroppedConnectionTry(_)
		| CoreError::ConnectionShutdown
		| CoreError::ConnectionUnavailable
		| CoreError::CannotInitCamera
		| CoreError::RelayTerminate
		| CoreError::CameraTerminate
		// Address resolution — DNS or IP parsing failed before we
		// ever got onto the wire.
		| CoreError::AddrResolutionError
		| CoreError::AddrParseError(_)
		// Timeout family — nothing came back in time at the
		// transport layer.
		| CoreError::Timeout(_)
		| CoreError::TimeoutError(_)
		| CoreError::TimeoutDisconnected
		| CoreError::BcUdpTimeout
		| CoreError::BcUdpReconnectTimeout
		// BcUdp drop family — UDP inner channel died.
		| CoreError::BcUdpDropReceiver(_)
		| CoreError::BcUdpDropSender
		| CoreError::BcUdpPayloadDroppedInner
		// Underlying std::io::Error bubbled up.
		| CoreError::Io(_) => EXIT_CONNECTION,
		// Camera answered truthfully that it does not expose the
		// requested ability (e.g. battery cams lack `ledState`).
		// This is a compatibility outcome, not a protocol bug.
		CoreError::MissingAbility { .. } => EXIT_UNSUPPORTED,
		// Everything not explicitly classified above is treated as a
		// protocol-layer failure. Add new core variants to the
		// connection or unsupported arms when appropriate.
		_ => EXIT_PROTOCOL,
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::Error as CoreError;

	#[test]
	fn classify_config_error() {
		let e: anyhow::Error = ConfigError::new("camera not found").into();
		assert_eq!(classify(&e), EXIT_CONFIG);
	}

	#[test]
	fn classify_usage_error() {
		let e: anyhow::Error = UsageError::new("snapshot stdout + --json").into();
		assert_eq!(classify(&e), EXIT_USAGE);
	}

	#[test]
	fn classify_interrupted() {
		let e: anyhow::Error = InterruptedError::new().into();
		assert_eq!(classify(&e), EXIT_INTERRUPTED);
	}

	#[test]
	fn classify_login_failure_is_connection() {
		let e: anyhow::Error = CoreError::CameraLoginFail.into();
		assert_eq!(classify(&e), EXIT_CONNECTION);
	}

	#[test]
	fn classify_dropped_connection_is_connection() {
		let e: anyhow::Error = CoreError::DroppedConnection.into();
		assert_eq!(classify(&e), EXIT_CONNECTION);
	}

	#[test]
	fn classify_service_unavailable_is_protocol() {
		let e: anyhow::Error = CoreError::CameraServiceUnavailable { id: 23, code: 500 }.into();
		assert_eq!(classify(&e), EXIT_PROTOCOL);
	}

	#[test]
	fn classify_unknown_is_unexpected() {
		let e: anyhow::Error = anyhow::anyhow!("something weird");
		assert_eq!(classify(&e), EXIT_UNEXPECTED);
	}

	#[test]
	fn classify_wrapped_error_walks_chain() {
		let e: anyhow::Error = anyhow::Error::from(ConfigError::new("missing"))
			.context("while loading config")
			.context("during bairelay startup");
		assert_eq!(classify(&e), EXIT_CONFIG);
	}

	#[test]
	fn classify_io_error_is_connection() {
		use std::sync::Arc;
		let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "no route");
		let e: anyhow::Error = CoreError::Io(Arc::new(io)).into();
		assert_eq!(classify(&e), EXIT_CONNECTION);
	}

	#[test]
	fn classify_auth_failure_is_connection() {
		// AuthFailed means the camera rejected our credentials — same
		// operator-facing family as CameraLoginFail, not a protocol bug.
		let e: anyhow::Error = CoreError::AuthFailed.into();
		assert_eq!(classify(&e), EXIT_CONNECTION);
	}

	#[test]
	fn classify_missing_ability_is_unsupported() {
		// Argus battery cams lack ledState; that's a compatibility
		// answer from the camera, not a protocol bug.
		let e: anyhow::Error = CoreError::MissingAbility {
			name: "ledState".into(),
			requested: "read".into(),
			actual: "none".into(),
		}
		.into();
		assert_eq!(classify(&e), EXIT_UNSUPPORTED);
	}
}
