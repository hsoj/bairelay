//! Driving a real Reolink camera over Baichuan.
//!
//! Implements every [`crate::camera`] role trait for the protocol
//! crate's [`BcCamera`], and is
//! the only place the BC wire vocabulary is translated into bairelay's:
//! a raw `voltage: i32` becomes [`Millivolts`], the ×1000 zoom
//! convention disappears behind [`ZoomLevel`], the six structurally
//! identical service-port RPC pairs collapse behind [`ServiceKind`],
//! and the `Support` XML report is reduced to [`CameraCapabilities`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::mpsc::Receiver;

use crate::baichuan::bc::xml::{
	AbilityInfo, FloodlightStatusList, LedState, RfAlarmCfg, UserList, VersionInfo,
};
use crate::baichuan::bc_protocol::{
	BcCamera, Direction, Error, LightState, MotionData, StreamKind, VideoStream,
};

use crate::battery::{BatteryStatus, Millivolts};
use crate::camera::{
	Camera, CameraResult, DeviceAdmin, Events, Lighting, Power, Ptz, Session, Stills, Video,
};
use crate::camera_services::{ServiceKind, ServicePortState};
use crate::capabilities::{ptz_mode_indicates_ptz, CameraCapabilities};
use crate::ptz::{PresetSlot, ZoomLevel};

/// Connect / login failure, classified here so the reconnect loop and
/// the one-shot runner never inspect baichuan error internals. The
/// baichuan cause stays on the `source` chain, which is what
/// `oneshot::classify` walks for its exit-code table.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
	/// The camera rejected the credentials. Terminal: retrying can
	/// never succeed, and hammers (on battery cameras, drains) the
	/// camera with doomed logins.
	#[error("authentication failed")]
	Auth(#[source] Error),
	/// Discovery, transport, or protocol failure before or during
	/// login. Retryable.
	#[error("connect failed")]
	Other(#[source] Error),
	/// A caller-supplied phase budget elapsed.
	#[error("{0} timed out")]
	Timeout(&'static str),
}

/// Split a failed login into terminal-vs-retryable. Only these two
/// variants mean "the camera answered and said no" — anything else
/// could be a battery camera still waking.
fn classify_login_error(e: Error) -> ConnectError {
	match e {
		Error::AuthFailed | Error::CameraLoginFail => ConnectError::Auth(e),
		e => ConnectError::Other(e),
	}
}

/// Connect and log in to `config`'s camera, returning it behind the
/// [`Camera`] port. No internal deadline — callers own the budget (the
/// reconnect loop wraps this in its `CONNECT_TIMEOUT` + cancel race;
/// the one-shot CLI budgets the phases separately via
/// [`connect_with_phase_timeouts`]).
pub async fn connect(
	config: &crate::config::CameraConfig,
) -> Result<Arc<dyn Camera>, ConnectError> {
	let camera = BcCamera::new(&crate::bc_opts::build_bc_opts(config))
		.await
		.map_err(ConnectError::Other)?;
	login(camera, config).await
}

/// [`connect`] with separate connect / login budgets. The one-shot CLI
/// gives discovery its ~80 s retry-loop budget but caps login far
/// tighter; the constants in `oneshot::runner` carry the rationale.
pub async fn connect_with_phase_timeouts(
	config: &crate::config::CameraConfig,
	connect_timeout: Duration,
	login_timeout: Duration,
) -> Result<Arc<dyn Camera>, ConnectError> {
	let camera = tokio::time::timeout(
		connect_timeout,
		BcCamera::new(&crate::bc_opts::build_bc_opts(config)),
	)
	.await
	.map_err(|_| ConnectError::Timeout("connect"))?
	.map_err(ConnectError::Other)?;
	tokio::time::timeout(login_timeout, login(camera, config))
		.await
		.map_err(|_| ConnectError::Timeout("login"))?
}

async fn login(
	camera: BcCamera,
	config: &crate::config::CameraConfig,
) -> Result<Arc<dyn Camera>, ConnectError> {
	camera
		.login_with_maxenc(crate::bc_opts::max_encryption(config))
		.await
		.map_err(classify_login_error)?;
	Ok(Arc::new(camera))
}

/// Translate the BC `BatteryInfo` XML into a [`BatteryStatus`].
/// Clamps percent to 100 once — some Argus firmwares briefly report
/// 101 on warm boot.
fn battery_status_from(info: crate::baichuan::bc::xml::BatteryInfo) -> BatteryStatus {
	BatteryStatus {
		percent: info.battery_percent.min(100) as u8,
		voltage: Millivolts(info.voltage),
		charge_status: info.charge_status,
		low_power: info.low_power != 0,
	}
}

#[async_trait]
impl Session for BcCamera {
	async fn end_session(&self) -> CameraResult<()> {
		BcCamera::logout(self).await
	}

	/// Lenient liveness probe over `get_linktype`. Some Reolink
	/// firmwares answer the link-type query with an XML payload that
	/// doesn't carry a `link_type` element; `BcCamera` surfaces that
	/// as [`Error::UnintelligibleReply`]. The TCP ACK proves the
	/// camera is alive, so that case maps to `Ok(())`. Every other
	/// error path is forwarded untouched.
	async fn keepalive_probe(&self) -> CameraResult<()> {
		match BcCamera::get_linktype(self).await {
			Ok(_) => Ok(()),
			Err(Error::UnintelligibleReply { .. }) => Ok(()),
			Err(e) => Err(e),
		}
	}
}

#[async_trait]
impl Video for BcCamera {
	async fn start_video(&self, kind: StreamKind) -> CameraResult<Box<dyn VideoStream>> {
		let stream = BcCamera::start_video(self, kind, 0, false).await?;
		Ok(Box::new(stream))
	}

	async fn stop_video(&self, kind: StreamKind) -> CameraResult<()> {
		BcCamera::stop_video(self, kind).await
	}
}

#[async_trait]
impl Stills for BcCamera {
	async fn snapshot(&self) -> CameraResult<Vec<u8>> {
		BcCamera::get_snapshot(self).await
	}
}

#[async_trait]
impl Events for BcCamera {
	async fn listen_on_motion(&self) -> CameraResult<MotionData> {
		BcCamera::listen_on_motion(self).await
	}

	async fn listen_on_floodlight(&self) -> CameraResult<Receiver<FloodlightStatusList>> {
		BcCamera::listen_on_floodlight(self).await
	}
}

#[async_trait]
impl Power for BcCamera {
	async fn battery_status(&self) -> CameraResult<BatteryStatus> {
		Ok(battery_status_from(BcCamera::battery_info(self).await?))
	}

	async fn pir_config(&self) -> CameraResult<RfAlarmCfg> {
		BcCamera::get_pirstate(self).await
	}

	async fn pir_set(&self, state: bool) -> CameraResult<()> {
		BcCamera::pir_set(self, state).await
	}
}

#[async_trait]
impl Lighting for BcCamera {
	async fn led_state(&self) -> CameraResult<LedState> {
		BcCamera::get_ledstate(self).await
	}

	async fn led_light_set(&self, state: bool) -> CameraResult<()> {
		BcCamera::led_light_set(self, state).await
	}

	async fn irled_light_set(&self, state: LightState) -> CameraResult<()> {
		BcCamera::irled_light_set(self, state).await
	}

	async fn is_floodlight_tasks_enabled(&self) -> CameraResult<bool> {
		BcCamera::is_floodlight_tasks_enabled(self).await
	}

	async fn floodlight_tasks_enable(&self, state: bool) -> CameraResult<()> {
		BcCamera::floodlight_tasks_enable(self, state).await
	}

	async fn set_floodlight_manual(&self, state: bool, duration: u16) -> CameraResult<()> {
		BcCamera::set_floodlight_manual(self, state, duration).await
	}

	async fn siren(&self) -> CameraResult<()> {
		BcCamera::siren(self).await
	}
}

#[async_trait]
impl Ptz for BcCamera {
	async fn send_ptz(&self, direction: Direction, amount: f32) -> CameraResult<()> {
		BcCamera::send_ptz(self, direction, amount).await
	}

	async fn ptz_presets(&self) -> CameraResult<Vec<PresetSlot>> {
		let ptz = BcCamera::get_ptz_preset(self).await?;
		Ok(ptz
			.preset_list
			.preset
			.into_iter()
			.map(|p| PresetSlot {
				id: p.id,
				name: p.name,
			})
			.collect())
	}

	async fn set_ptz_preset(&self, preset_id: u8, name: String) -> CameraResult<()> {
		BcCamera::set_ptz_preset(self, preset_id, name).await
	}

	async fn moveto_ptz_preset(&self, preset_id: u8) -> CameraResult<()> {
		BcCamera::moveto_ptz_preset(self, preset_id).await
	}

	async fn zoom_to(&self, level: ZoomLevel) -> CameraResult<()> {
		BcCamera::zoom_to(self, level.camera_units()).await
	}
}

#[async_trait]
impl DeviceAdmin for BcCamera {
	async fn capabilities(&self) -> CameraResult<CameraCapabilities> {
		let support = BcCamera::get_support(self).await?;
		Ok(CameraCapabilities {
			has_ptz: ptz_mode_indicates_ptz(support.ptz_mode.as_deref(), support.ptz_cfg),
		})
	}

	async fn version(&self) -> CameraResult<VersionInfo> {
		BcCamera::version(self).await
	}

	async fn ability_info(&self) -> CameraResult<AbilityInfo> {
		BcCamera::get_abilityinfo(self).await
	}

	async fn reboot(&self) -> CameraResult<()> {
		BcCamera::reboot(self).await
	}

	async fn set_time(&self, timestamp: OffsetDateTime) -> CameraResult<()> {
		BcCamera::set_time(self, timestamp).await
	}

	async fn users(&self) -> CameraResult<UserList> {
		BcCamera::get_users(self).await
	}

	async fn add_user(
		&self,
		user_name: String,
		password: String,
		user_level: u8,
	) -> CameraResult<()> {
		BcCamera::add_user(self, user_name, password, user_level).await
	}

	async fn modify_user(&self, user_name: String, password: String) -> CameraResult<()> {
		BcCamera::modify_user(self, user_name, password).await
	}

	async fn delete_user(&self, user_name: String) -> CameraResult<()> {
		BcCamera::delete_user(self, user_name).await
	}

	async fn service(&self, kind: ServiceKind) -> CameraResult<ServicePortState> {
		let (port, enable) = match kind {
			ServiceKind::Baichuan => {
				let s = BcCamera::get_serverport(self).await?;
				(s.port, s.enable)
			}
			ServiceKind::Http => {
				let s = BcCamera::get_http(self).await?;
				(s.port, s.enable)
			}
			ServiceKind::Https => {
				let s = BcCamera::get_https(self).await?;
				(s.port, s.enable)
			}
			ServiceKind::Rtmp => {
				let s = BcCamera::get_rtmp(self).await?;
				(s.port, s.enable)
			}
			ServiceKind::Rtsp => {
				let s = BcCamera::get_rtsp(self).await?;
				(s.port, s.enable)
			}
			ServiceKind::Onvif => {
				let s = BcCamera::get_onvif(self).await?;
				(s.port, s.enable)
			}
		};
		Ok(ServicePortState {
			port,
			enabled: enable.map(|e| e != 0),
		})
	}

	async fn set_service(
		&self,
		kind: ServiceKind,
		enable: Option<bool>,
		port: Option<u32>,
	) -> CameraResult<()> {
		match kind {
			ServiceKind::Baichuan => BcCamera::set_serverport(self, enable, port).await,
			ServiceKind::Http => BcCamera::set_http(self, enable, port).await,
			ServiceKind::Https => BcCamera::set_https(self, enable, port).await,
			ServiceKind::Rtmp => BcCamera::set_rtmp(self, enable, port).await,
			ServiceKind::Rtsp => BcCamera::set_rtsp(self, enable, port).await,
			ServiceKind::Onvif => BcCamera::set_onvif(self, enable, port).await,
		}
	}
}

#[cfg(test)]
mod connect_tests {
	use super::*;

	/// Both typed login rejections — and only those — are terminal.
	/// This is the contract the reconnect loop's permanent-bail rides
	/// on (auth failures stop retries; anything else backs off).
	#[test]
	fn classify_login_error_auth_variants_are_terminal() {
		assert!(matches!(
			classify_login_error(Error::AuthFailed),
			ConnectError::Auth(_)
		));
		assert!(matches!(
			classify_login_error(Error::CameraLoginFail),
			ConnectError::Auth(_)
		));
	}

	#[test]
	fn classify_login_error_transport_failure_is_retryable() {
		assert!(matches!(
			classify_login_error(Error::DroppedConnection),
			ConnectError::Other(_)
		));
	}

	/// `oneshot::classify` picks exit codes by downcasting the baichuan
	/// error out of the anyhow chain — the wrapper must keep it
	/// reachable as a `source`.
	#[test]
	fn connect_error_keeps_baichuan_cause_on_the_chain() {
		let e = anyhow::Error::from(classify_login_error(Error::AuthFailed));
		assert!(e
			.chain()
			.any(|c| matches!(c.downcast_ref::<Error>(), Some(Error::AuthFailed))));
	}
}

#[cfg(test)]
mod tests {
	//! Cover every forwarding line in the `Camera for BcCamera` adapter.
	//! Each method is invoked through `&dyn Camera` against a
	//! `MockConnection`-backed `BcCamera`. We don't care about the
	//! success / failure outcome — only that the adapter line executes
	//! once per method. Errors are accepted (most methods fail ability
	//! gates or get an err_code 500 reply) because the goal is to drive
	//! the forwarding + translation body, not to re-test the wrapped
	//! command's happy path.
	use super::*;
	use crate::baichuan::bc_protocol::connection::mock::{reply_err_code, MockConnection};
	use crate::camera::Camera;
	use std::sync::Arc;
	use std::time::Duration;

	/// Build a BcCamera whose connection accepts every msg_id and never
	/// replies — the 200 ms timeout in [`run`] bounds each call.
	async fn err_camera() -> BcCamera {
		let mock = MockConnection::new().build().await;
		BcCamera::from_mock_connection(mock).await
	}

	async fn run<F, T>(fut: F) -> std::result::Result<T, &'static str>
	where
		F: std::future::Future<Output = T>,
	{
		tokio::time::timeout(Duration::from_millis(200), fut)
			.await
			.map_err(|_| "timeout")
	}

	#[tokio::test]
	async fn adapter_drives_every_method() {
		use crate::baichuan::bc::model::*;

		// listen_on_motion: hits ability check → Err.
		{
			let cam = err_camera().await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.listen_on_motion()).await;
		}
		// battery_status: err_code 500 → the translation line is
		// unreached; a real-reply test lives in the fixture suite.
		{
			let mock = MockConnection::new()
				.expect_msg(MSG_ID_BATTERY_INFO)
				.reply_with(|req| reply_err_code(req, 500))
				.build()
				.await;
			let cam = BcCamera::from_mock_connection(mock).await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.battery_status()).await;
		}
		// pir_config: needs MSG_ID_GET_PIR_ALARM.
		{
			let mock = MockConnection::new()
				.expect_msg(MSG_ID_GET_PIR_ALARM)
				.reply_with(|req| reply_err_code(req, 500))
				.build()
				.await;
			let cam = BcCamera::from_mock_connection(mock).await;
			cam.test_set_ability("rfAlarm", true).await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.pir_config()).await;
		}
		// Side-effect + ability-gated commands: hit the ability gate
		// (→ Err) but the adapter line runs.
		{
			let cam = err_camera().await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.pir_set(true)).await;
			let _ = run(dr.listen_on_floodlight()).await;
			let _ = run(dr.is_floodlight_tasks_enabled()).await;
			let _ = run(dr.floodlight_tasks_enable(true)).await;
			let _ = run(dr.set_floodlight_manual(true, 30)).await;
			let _ = run(dr.send_ptz(Direction::Stop, 1.0)).await;
			let _ = run(dr.set_ptz_preset(1, "x".to_string())).await;
			let _ = run(dr.moveto_ptz_preset(1)).await;
			let _ = run(dr.zoom_to(ZoomLevel::from_factor(1.5))).await;
			let _ = run(dr.led_light_set(true)).await;
			let _ = run(dr.irled_light_set(LightState::Off)).await;
			let _ = run(dr.reboot()).await;
			let _ = run(dr.siren()).await;
			let _ = run(dr.led_state()).await;
			let _ = run(dr.ptz_presets()).await;
			let _ = run(dr.start_video(StreamKind::Main)).await;
			let _ = run(dr.end_session()).await;
			let now = time::OffsetDateTime::now_utc();
			let _ = run(dr.set_time(now)).await;
		}
		// snapshot: err_code 500.
		{
			let mock = MockConnection::new()
				.expect_msg(MSG_ID_SNAP)
				.reply_with(|req| reply_err_code(req, 500))
				.build()
				.await;
			let cam = BcCamera::from_mock_connection(mock).await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.snapshot()).await;
		}
		// version: err_code 500.
		{
			let mock = MockConnection::new()
				.expect_msg(MSG_ID_VERSION)
				.reply_with(|req| reply_err_code(req, 500))
				.build()
				.await;
			let cam = BcCamera::from_mock_connection(mock).await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.version()).await;
		}
		// users: err_code 500.
		{
			let mock = MockConnection::new()
				.expect_msg(MSG_ID_UPDATE_USER_LIST)
				.reply_with(|req| reply_err_code(req, 500))
				.build()
				.await;
			let cam = BcCamera::from_mock_connection(mock).await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.users()).await;
		}
		// add/modify/delete user: send-then-recv pends → 200 ms timeout.
		{
			let cam = err_camera().await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.add_user("u".to_string(), "p".to_string(), 1)).await;
			let _ = run(dr.modify_user("u".to_string(), "p".to_string())).await;
			let _ = run(dr.delete_user("u".to_string())).await;
		}
		// service / set_service: every kind through the one match.
		{
			let cam = err_camera().await;
			let dr: &dyn Camera = &cam;
			for kind in ServiceKind::ALL {
				let _ = run(dr.service(kind)).await;
				let _ = run(dr.set_service(kind, Some(true), None)).await;
			}
		}
		// keepalive_probe (wraps get_linktype): recv pends → timeout.
		{
			let cam = err_camera().await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.keepalive_probe()).await;
		}
		// capabilities (wraps get_support): recv pends → timeout.
		{
			let cam = err_camera().await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.capabilities()).await;
		}
		// ability_info: err_code 500.
		{
			let mock = MockConnection::new()
				.expect_msg(MSG_ID_ABILITY_INFO)
				.reply_with(|req| reply_err_code(req, 500))
				.build()
				.await;
			let cam = BcCamera::from_mock_connection(mock).await;
			let dr: &dyn Camera = &cam;
			let _ = run(dr.ability_info()).await;
		}
	}

	#[test]
	fn battery_status_translation_clamps_and_types_units() {
		use crate::baichuan::bc::xml::BatteryInfo;

		let status = battery_status_from(BatteryInfo {
			battery_percent: 101,
			voltage: 3942,
			charge_status: "charging".into(),
			low_power: 1,
			..Default::default()
		});
		assert_eq!(status.percent, 100);
		assert_eq!(status.voltage, Millivolts(3942));
		assert_eq!(status.charge_status, "charging");
		assert!(status.low_power);

		let plain = battery_status_from(BatteryInfo {
			battery_percent: 87,
			low_power: 0,
			..Default::default()
		});
		assert_eq!(plain.percent, 87);
		assert!(!plain.low_power);
	}

	#[tokio::test]
	async fn dyn_camera_arc_coercion_holds() {
		let cam = Arc::new(err_camera().await);
		let _dr: Arc<dyn Camera> = cam;
	}

	#[tokio::test]
	async fn keepalive_probe_treats_unintelligible_as_ok() {
		use crate::baichuan::bc::model::MSG_ID_PING;
		use crate::baichuan::bc::xml::BcXml;
		use crate::baichuan::bc_protocol::connection::mock::reply_200_xml;

		// Older firmware path: link-type query returns 200 with a
		// payload missing `link_type`. `get_linktype` surfaces
		// `UnintelligibleReply`; the adapter's probe must swallow that
		// and report the camera alive.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let dr: &dyn Camera = &cam;
		dr.keepalive_probe().await.expect("unintelligible → Ok");
	}

	#[tokio::test]
	async fn keepalive_probe_happy_path_is_ok() {
		use crate::baichuan::bc::model::MSG_ID_PING;
		use crate::baichuan::bc::xml::{BcXml, LinkType};
		use crate::baichuan::bc_protocol::connection::mock::reply_200_xml;

		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						link_type: Some(LinkType {
							link_type: "LAN".to_string(),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let dr: &dyn Camera = &cam;
		dr.keepalive_probe().await.expect("ok");
	}

	#[tokio::test]
	async fn keepalive_probe_forwards_other_errors() {
		use crate::baichuan::bc::model::MSG_ID_PING;

		// Non-200 reply must not be swallowed by the lenient wrapper —
		// only `UnintelligibleReply` gets the special treatment.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PING)
			.reply_with(|req| reply_err_code(req, 400))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let dr: &dyn Camera = &cam;
		let err = dr.keepalive_probe().await.expect_err("400 → err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 400, .. }
		));
	}
}
