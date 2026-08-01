use anyhow::{Context, Result};

use super::errors::UsageError;
use super::output::{Outcome, ServiceEntry};
use crate::camera::Camera;
use crate::camera_services::ServiceKind;

/// What to do with the service: read, enable, disable, set port, or both.
///
/// Port is `u16` (matching the CLI surface in `cli.rs::ServiceAction`).
/// `0` is rejected at dispatch time as `UsageError` (operator typo
/// guard); the cast to `u32` happens at the port boundary since the
/// camera-side `set_service(…, port: Option<u32>)` takes a wider type.
#[derive(Debug, Clone)]
pub enum Action {
	Get,
	On,
	Off,
	Port(u16),
	Set { port: u16, enabled: bool },
}

pub async fn run(cam: &dyn Camera, service: ServiceKind, action: Action) -> Result<Outcome> {
	// Apply the mutation first (if any), then always read back so the
	// returned Outcome reflects the camera's current state.
	match &action {
		Action::Get => {}
		Action::On => apply_toggle(cam, service, Some(true), None).await?,
		Action::Off => apply_toggle(cam, service, Some(false), None).await?,
		Action::Port(p) => apply_toggle(cam, service, None, Some(*p)).await?,
		Action::Set { port, enabled } => {
			apply_toggle(cam, service, Some(*enabled), Some(*port)).await?
		}
	}

	let state = cam
		.service(service)
		.await
		.with_context(|| format!("read {service} service failed"))?;
	Ok(Outcome::Service {
		service: service.label().into(),
		port: state.port,
		enabled: state.enabled,
	})
}

/// Per-RPC timeout for `run_all`'s six sequential reads. The outer
/// `runner::OP_TIMEOUT = 30 s` covers the whole call; without an
/// inner cap a single hung RPC consumes the entire budget and the
/// operator gets zero partial results. 8 s × 6 = 48 s worst case,
/// 2 s above the outer cap — runner returns ConnectionTimeout
/// before the last RPC fires, which is fine: the operator sees a
/// useful "5 of 6 services answered" output.
const PER_RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Read every service in one shot. Used by `bairelay services <cam>`
/// with no positional service argument. Each failed service becomes an
/// entry with `enabled = None` rather than failing the whole call —
/// older firmwares don't expose all six on every channel, and any
/// individual RPC that exceeds [`PER_RPC_TIMEOUT`] degrades to the
/// same "unknown" entry instead of starving the remaining reads.
pub async fn run_all(cam: &dyn Camera) -> Result<Outcome> {
	let mut services = Vec::with_capacity(ServiceKind::ALL.len());
	for svc in ServiceKind::ALL {
		match tokio::time::timeout(PER_RPC_TIMEOUT, cam.service(svc)).await {
			Ok(Ok(state)) => services.push(ServiceEntry {
				name: svc.label().into(),
				port: state.port,
				enabled: state.enabled,
			}),
			Ok(Err(_)) | Err(_) => services.push(ServiceEntry {
				name: svc.label().into(),
				port: 0,
				enabled: None,
			}),
		}
	}
	Ok(Outcome::ServiceList { services })
}

async fn apply_toggle(
	cam: &dyn Camera,
	service: ServiceKind,
	set_on: Option<bool>,
	set_port: Option<u16>,
) -> Result<()> {
	if matches!(set_port, Some(0)) {
		return Err(UsageError::new(format!("port 0 is not valid for {}", service.label())).into());
	}
	cam.set_service(service, set_on, set_port.map(u32::from))
		.await
		.with_context(|| format!("set {service} service failed"))?;
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::Error;
	use crate::camera_services::ServicePortState;
	use crate::fake_camera::FakeCameraBuilder;

	fn all_enabled_fake() -> std::sync::Arc<crate::fake_camera::FakeCamera> {
		FakeCameraBuilder::new()
			.with_service(|kind| {
				Ok(match kind {
					ServiceKind::Baichuan => ServicePortState {
						port: 9000,
						enabled: Some(true),
					},
					ServiceKind::Http => ServicePortState {
						port: 80,
						enabled: Some(true),
					},
					ServiceKind::Https => ServicePortState {
						port: 443,
						enabled: Some(false),
					},
					ServiceKind::Rtsp => ServicePortState {
						port: 554,
						enabled: Some(true),
					},
					ServiceKind::Rtmp => ServicePortState {
						port: 1935,
						enabled: Some(true),
					},
					ServiceKind::Onvif => ServicePortState {
						port: 8000,
						enabled: Some(true),
					},
				})
			})
			.build()
	}

	#[tokio::test]
	async fn service_label_all_variants() {
		assert_eq!(ServiceKind::Baichuan.label(), "baichuan");
		assert_eq!(ServiceKind::Http.label(), "http");
		assert_eq!(ServiceKind::Https.label(), "https");
		assert_eq!(ServiceKind::Rtmp.label(), "rtmp");
		assert_eq!(ServiceKind::Rtsp.label(), "rtsp");
		assert_eq!(ServiceKind::Onvif.label(), "onvif");
	}

	#[tokio::test]
	async fn run_get_http_returns_current_state() {
		let fake = all_enabled_fake();
		let outcome = run(&*fake, ServiceKind::Http, Action::Get).await.unwrap();
		assert_eq!(
			outcome,
			Outcome::Service {
				service: "http".into(),
				port: 80,
				enabled: Some(true),
			}
		);
	}

	#[tokio::test]
	async fn run_on_baichuan_records_set_call() {
		let fake = all_enabled_fake();
		let _ = run(&*fake, ServiceKind::Baichuan, Action::On)
			.await
			.unwrap();
		assert_eq!(
			*fake.calls().set_service.lock().unwrap(),
			vec![(ServiceKind::Baichuan, Some(true), None)]
		);
	}

	#[tokio::test]
	async fn run_off_https_records_set_call() {
		let fake = all_enabled_fake();
		let _ = run(&*fake, ServiceKind::Https, Action::Off).await.unwrap();
		assert_eq!(
			*fake.calls().set_service.lock().unwrap(),
			vec![(ServiceKind::Https, Some(false), None)]
		);
	}

	#[tokio::test]
	async fn run_port_rtsp_records_set_call() {
		let fake = all_enabled_fake();
		let _ = run(&*fake, ServiceKind::Rtsp, Action::Port(8554))
			.await
			.unwrap();
		assert_eq!(
			*fake.calls().set_service.lock().unwrap(),
			vec![(ServiceKind::Rtsp, None, Some(8554))]
		);
	}

	#[tokio::test]
	async fn run_set_rtmp_records_both_args() {
		let fake = all_enabled_fake();
		let _ = run(
			&*fake,
			ServiceKind::Rtmp,
			Action::Set {
				port: 1935,
				enabled: false,
			},
		)
		.await
		.unwrap();
		assert_eq!(
			*fake.calls().set_service.lock().unwrap(),
			vec![(ServiceKind::Rtmp, Some(false), Some(1935))]
		);
	}

	#[tokio::test]
	async fn run_onvif_set_records_call() {
		let fake = all_enabled_fake();
		let _ = run(&*fake, ServiceKind::Onvif, Action::On).await.unwrap();
		assert_eq!(
			*fake.calls().set_service.lock().unwrap(),
			vec![(ServiceKind::Onvif, Some(true), None)]
		);
	}

	#[tokio::test]
	async fn run_http_set_records_call() {
		let fake = all_enabled_fake();
		let _ = run(&*fake, ServiceKind::Http, Action::On).await.unwrap();
		assert_eq!(
			*fake.calls().set_service.lock().unwrap(),
			vec![(ServiceKind::Http, Some(true), None)]
		);
	}

	#[tokio::test]
	async fn run_all_lists_all_six_services() {
		let fake = all_enabled_fake();
		let outcome = run_all(&*fake).await.unwrap();
		let Outcome::ServiceList { services } = outcome else {
			panic!("wrong variant");
		};
		assert_eq!(services.len(), 6);
		let names: Vec<_> = services.iter().map(|s| s.name.as_str()).collect();
		assert_eq!(
			names,
			vec!["baichuan", "http", "https", "rtmp", "rtsp", "onvif"]
		);
	}

	#[tokio::test]
	async fn run_all_failing_service_becomes_unknown_entry() {
		let fake = FakeCameraBuilder::new()
			.with_service(|kind| match kind {
				ServiceKind::Http => Ok(ServicePortState {
					port: 80,
					enabled: Some(true),
				}),
				ServiceKind::Rtsp => Ok(ServicePortState {
					port: 554,
					enabled: None,
				}),
				_ => Err(Error::Other("service down")),
			})
			.build();
		let outcome = run_all(&*fake).await.unwrap();
		let Outcome::ServiceList { services } = outcome else {
			panic!("wrong variant");
		};
		// baichuan failed
		assert_eq!(services[0].name, "baichuan");
		assert_eq!(services[0].port, 0);
		assert!(services[0].enabled.is_none());
		// http ok
		assert_eq!(services[1].name, "http");
		assert_eq!(services[1].port, 80);
		assert_eq!(services[1].enabled, Some(true));
		// rtsp port returned but enable=None
		assert_eq!(services[4].name, "rtsp");
		assert_eq!(services[4].port, 554);
		assert!(services[4].enabled.is_none());
	}

	#[tokio::test]
	async fn run_port_zero_is_usage_error() {
		// Operator typo guard: `bairelay services cam http port 0` would
		// otherwise reach the camera and surface as an opaque
		// camera-side rejection. Map to UsageError → EXIT_USAGE = 2 so
		// CI scripts can branch cleanly.
		let fake = all_enabled_fake();
		let err = run(&*fake, ServiceKind::Http, Action::Port(0))
			.await
			.expect_err("port 0 must be rejected");
		assert!(format!("{err:#}").contains("port 0 is not valid"));
		// Camera was never asked.
		assert!(fake.calls().set_service.lock().unwrap().is_empty());
	}

	#[tokio::test]
	async fn run_set_with_port_zero_is_usage_error() {
		let fake = all_enabled_fake();
		let err = run(
			&*fake,
			ServiceKind::Rtsp,
			Action::Set {
				port: 0,
				enabled: true,
			},
		)
		.await
		.expect_err("port 0 must be rejected");
		assert!(format!("{err:#}").contains("port 0 is not valid"));
		assert!(fake.calls().set_service.lock().unwrap().is_empty());
	}
}
