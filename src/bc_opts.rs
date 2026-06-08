//! `BcCameraOpt` builder shared by the camera connect loop and CLI.

use crate::config::{self, CameraConfig};
use bairelay_neolink_core::bc_protocol::{
	BcCameraOpt, ConnectionProtocol, Credentials, DiscoveryMethods, MaxEncryption,
};
use std::net::IpAddr;

/// Build a `BcCameraOpt` from the service `CameraConfig` using the same
/// rules the long-running camera connect loop uses.
pub fn build_bc_opts(cfg: &CameraConfig) -> BcCameraOpt {
	let mut addrs = vec![];
	let mut port = None;

	if let Some(ref addr_str) = cfg.address {
		// Parse "host:port" format
		if let Some((host, p)) = addr_str.rsplit_once(':') {
			if let Ok(ip) = host.parse::<IpAddr>() {
				addrs.push(ip);
			}
			port = p.parse().ok();
		} else if let Ok(ip) = addr_str.parse::<IpAddr>() {
			addrs.push(ip);
		}
	}

	let discovery = match cfg.discovery {
		config::DiscoveryMethod::Local => DiscoveryMethods::Local,
		config::DiscoveryMethod::Remote => DiscoveryMethods::Remote,
		config::DiscoveryMethod::Map => DiscoveryMethods::Map,
		config::DiscoveryMethod::Relay => DiscoveryMethods::Relay,
		config::DiscoveryMethod::Cellular => DiscoveryMethods::Cellular,
	};

	BcCameraOpt {
		name: cfg.name.clone(),
		channel_id: cfg.channel_id,
		addrs,
		uid: cfg.uid.clone(),
		port,
		protocol: ConnectionProtocol::TcpUdp,
		discovery,
		max_discovery_retries: 10,
		credentials: Credentials {
			username: cfg.username.clone(),
			password: cfg.password.clone(),
		},
		debug: false,
	}
}

/// Translate the config-level max-encryption enum into the bairelay_neolink_core
/// value that `BcCamera::login_with_maxenc` expects.
pub fn max_encryption(cfg: &CameraConfig) -> MaxEncryption {
	match cfg.max_encryption {
		config::MaxEncryption::None => MaxEncryption::None,
		config::MaxEncryption::Aes => MaxEncryption::Aes,
		config::MaxEncryption::BcEncrypt => MaxEncryption::BcEncrypt,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample_cfg() -> CameraConfig {
		toml::from_str(
			r#"
			name = "driveway"
			username = "admin"
			password = "s3cret"
			address = "192.168.1.187:9000"
			uid = "9527000TESTCAM00"
			channel_id = 0
			stream = "all"
			discovery = "relay"
			max_encryption = "aes"
			idle_disconnect = true
			enabled = true
			"#,
		)
		.unwrap()
	}

	#[test]
	fn build_bc_opts_pins_every_field() {
		let cfg = sample_cfg();
		let opts = build_bc_opts(&cfg);

		assert_eq!(opts.name, "driveway");
		assert_eq!(opts.channel_id, 0);
		assert_eq!(opts.uid.as_deref(), Some("9527000TESTCAM00"));
		assert_eq!(opts.port, Some(9000));
		assert!(matches!(opts.protocol, ConnectionProtocol::TcpUdp));
		assert!(matches!(opts.discovery, DiscoveryMethods::Relay));
		assert_eq!(opts.max_discovery_retries, 10);
		assert_eq!(opts.credentials.username, "admin");
		assert_eq!(opts.credentials.password.as_deref(), Some("s3cret"));
		assert!(!opts.debug);
		// Address parse: 192.168.1.187 must be present.
		assert!(opts.addrs.iter().any(|a| a.to_string() == "192.168.1.187"));
	}

	#[test]
	fn max_encryption_maps_every_variant() {
		// sample_cfg() uses max_encryption = "aes".
		assert!(matches!(max_encryption(&sample_cfg()), MaxEncryption::Aes));

		let none_cfg: CameraConfig = toml::from_str(
			r#"
			name = "c"
			username = "u"
			address = "1.2.3.4:9000"
			channel_id = 0
			stream = "all"
			discovery = "local"
			max_encryption = "none"
			"#,
		)
		.unwrap();
		assert!(matches!(max_encryption(&none_cfg), MaxEncryption::None));

		let bc_cfg: CameraConfig = toml::from_str(
			r#"
			name = "c"
			username = "u"
			address = "1.2.3.4:9000"
			channel_id = 0
			stream = "all"
			discovery = "local"
			max_encryption = "bcencrypt"
			"#,
		)
		.unwrap();
		assert!(matches!(max_encryption(&bc_cfg), MaxEncryption::BcEncrypt));
	}

	#[test]
	fn build_bc_opts_handles_bare_ip_without_port() {
		let cfg: CameraConfig = toml::from_str(
			r#"
			name = "bare"
			username = "u"
			address = "10.0.0.5"
			channel_id = 0
			stream = "all"
			discovery = "local"
			max_encryption = "none"
			"#,
		)
		.unwrap();
		let opts = build_bc_opts(&cfg);

		assert_eq!(opts.port, None);
		assert!(opts.addrs.iter().any(|a| a.to_string() == "10.0.0.5"));
	}

	#[test]
	fn build_bc_opts_maps_each_discovery_variant() {
		// Cover every arm of the `cfg.discovery → DiscoveryMethods`
		// match so a future addition forces this table to grow.
		let tests = [
			("local", DiscoveryMethods::Local),
			("remote", DiscoveryMethods::Remote),
			("map", DiscoveryMethods::Map),
			("relay", DiscoveryMethods::Relay),
			("cellular", DiscoveryMethods::Cellular),
		];
		for (name, expected) in tests {
			let cfg: CameraConfig = toml::from_str(&format!(
				r#"
				name = "c"
				username = "u"
				address = "1.2.3.4:9000"
				channel_id = 0
				stream = "all"
				discovery = "{name}"
				max_encryption = "none"
				"#
			))
			.unwrap();
			let opts = build_bc_opts(&cfg);
			assert!(
				std::mem::discriminant(&opts.discovery) == std::mem::discriminant(&expected),
				"discovery {name} should map to {expected:?}"
			);
		}
	}

	#[test]
	fn build_bc_opts_handles_uid_only_no_address() {
		let cfg: CameraConfig = toml::from_str(
			r#"
			name = "uid-only"
			username = "u"
			uid = "ABC123"
			channel_id = 0
			stream = "all"
			discovery = "relay"
			max_encryption = "none"
			"#,
		)
		.unwrap();
		let opts = build_bc_opts(&cfg);

		assert!(opts.addrs.is_empty());
		assert_eq!(opts.port, None);
		assert_eq!(opts.uid.as_deref(), Some("ABC123"));
	}
}
