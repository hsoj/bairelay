//! A scripted stand-in for a real camera.
//!
//! Backed by closures the test sets per method. Unset read/stream
//! methods panic with a clear diagnostic so tests fail fast rather than
//! silently returning bogus data. Side-effect setters (`pir_set`,
//! `reboot`, …) record their arguments and invocation counts in
//! [`FakeCalls`] for assertion.
//!
//! `#[cfg(test)]`-gated in `lib.rs`, so production builds cannot link
//! it and substitute a fake for a real camera (`TS-2`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::mpsc::Receiver;

use crate::baichuan::bc::xml::{
	AbilityInfo, FloodlightStatusList, LedState, RfAlarmCfg, UserList, VersionInfo,
};
use crate::baichuan::bc_protocol::{
	Direction, Error, LightState, MotionData, StreamKind, VideoStream,
};

use crate::battery::BatteryStatus;
use crate::camera::{Camera, CameraResult};
use crate::camera_services::{ServiceKind, ServicePortState};
use crate::capabilities::CameraCapabilities;
use crate::ptz::{PresetSlot, ZoomLevel};

type BoxFn<T> = Box<dyn Fn() -> CameraResult<T> + Send + Sync>;

/// One recorded `set_service` call: which service, and the enable /
/// port fields the caller asked to change (`None` = leave alone).
type SetServiceCall = (ServiceKind, Option<bool>, Option<u32>);

/// Call log populated by side-effect methods on [`FakeCamera`]. Tests
/// read these fields after exercising the code under test to confirm
/// the right camera calls were made with the right arguments.
#[derive(Default)]
pub struct FakeCalls {
	/// Count of `reboot` invocations.
	pub reboot: Mutex<u32>,
	/// Count of `siren` invocations.
	pub siren: Mutex<u32>,
	/// Count of `end_session` invocations.
	pub end_session: Mutex<u32>,
	/// Each `pir_set(state)` call appended as `state`.
	pub pir_set: Mutex<Vec<bool>>,
	/// Each `floodlight_tasks_enable(state)` call appended as `state`.
	pub floodlight_tasks_enable: Mutex<Vec<bool>>,
	/// Each `set_floodlight_manual(state, duration)` call appended.
	pub set_floodlight_manual: Mutex<Vec<(bool, u16)>>,
	/// Each `send_ptz(direction, amount)` call appended.
	pub send_ptz: Mutex<Vec<(Direction, f32)>>,
	/// Each `set_ptz_preset(id, name)` call appended.
	pub set_ptz_preset: Mutex<Vec<(u8, String)>>,
	/// Each `moveto_ptz_preset(id)` call appended.
	pub moveto_ptz_preset: Mutex<Vec<u8>>,
	/// Each `zoom_to(level)` call appended.
	pub zoom_to: Mutex<Vec<ZoomLevel>>,
	/// Each `led_light_set(state)` call appended.
	pub led_light_set: Mutex<Vec<bool>>,
	/// Each `irled_light_set(state)` call appended.
	pub irled_light_set: Mutex<Vec<LightState>>,
	/// Each `set_time(ts)` call appended.
	pub set_time: Mutex<Vec<OffsetDateTime>>,
	/// Each `add_user(name, password, level)` call appended.
	pub add_user: Mutex<Vec<(String, String, u8)>>,
	/// Each `modify_user(name, password)` call appended.
	pub modify_user: Mutex<Vec<(String, String)>>,
	/// Each `delete_user(name)` call appended.
	pub delete_user: Mutex<Vec<String>>,
	/// Each `set_service(kind, enable, port)` call appended.
	pub set_service: Mutex<Vec<SetServiceCall>>,
	/// Each `start_video(kind)` call appended. Recorded even when the
	/// scripted stream is an error, so a test can wait for the reader
	/// task to have passed its start-up race before acting.
	pub start_video: Mutex<Vec<StreamKind>>,
	/// Each `stop_video(kind)` call appended.
	pub stop_video: Mutex<Vec<StreamKind>>,
}

/// Internal state backing a [`FakeCamera`]. Read-method closures are
/// `Option<BoxFn<_>>`; stream-returning methods hold pre-built values
/// in a `Mutex<Option<_>>` so the first call takes ownership.
///
/// `*_pending` flags request the matching async method to never
/// resolve (returns `pending()`); used by mqtt_dispatch tests to
/// drive the 30 s `tokio::time::timeout` warn-then-OK branch under
/// `tokio::time::pause()`.
#[derive(Default)]
struct FakeState {
	battery_status: Option<BoxFn<BatteryStatus>>,
	battery_status_pending: bool,
	pir_config: Option<BoxFn<RfAlarmCfg>>,
	pir_config_pending: bool,
	is_floodlight_tasks_enabled: Option<BoxFn<bool>>,
	snapshot: Option<BoxFn<Vec<u8>>>,
	snapshot_pending: bool,
	send_ptz_pending: bool,
	siren_pending: bool,
	is_floodlight_tasks_enabled_pending: bool,
	ptz_presets_pending: bool,
	led_state: Option<BoxFn<LedState>>,
	ptz_presets: Option<BoxFn<Vec<PresetSlot>>>,
	version: Option<BoxFn<VersionInfo>>,
	users: Option<BoxFn<UserList>>,
	service: Option<Box<dyn Fn(ServiceKind) -> CameraResult<ServicePortState> + Send + Sync>>,
	keepalive_probe: Option<BoxFn<()>>,
	capabilities: Option<BoxFn<CameraCapabilities>>,
	ability_info: Option<BoxFn<AbilityInfo>>,
	motion_stream: Mutex<Option<MotionData>>,
	floodlight_stream: Mutex<Option<Receiver<FloodlightStatusList>>>,
	video_stream: Mutex<Option<Box<dyn VideoStream>>>,
	/// When `Some`, `listen_on_motion` returns `Err(f())` on every call
	/// rather than consuming the `motion_stream` value.
	motion_stream_error: Option<Box<dyn Fn() -> Error + Send + Sync>>,
	/// When `Some`, `listen_on_floodlight` returns `Err(f())` on every
	/// call rather than consuming the `floodlight_stream` value.
	floodlight_stream_error: Option<Box<dyn Fn() -> Error + Send + Sync>>,
	/// When `Some`, `start_video` returns `Err(f())` on every call
	/// rather than consuming the `video_stream` value.
	video_stream_error: Option<Box<dyn Fn() -> Error + Send + Sync>>,
	calls: FakeCalls,
}

/// A [`Camera`] implementation driven entirely by closures and
/// pre-built stream values configured on the [`FakeCameraBuilder`].
///
/// Access the call log via [`FakeCamera::calls`] after the code under
/// test has run. An unconfigured read or stream method panics with a
/// `"FakeCamera: <method> not configured for this test"` message —
/// silent defaults are deliberately not supported because they hide
/// test-setup bugs.
pub struct FakeCamera {
	inner: Arc<FakeState>,
}

impl FakeCamera {
	/// Access the call log recorded by side-effect methods.
	pub fn calls(&self) -> &FakeCalls {
		&self.inner.calls
	}
}

/// Builder for a [`FakeCamera`]. Use `with_<method>(...)` to install a
/// closure per read method, or `with_<method>_stream(...)` to install
/// a pre-built value for a stream-returning method. Call [`build`] to
/// produce an `Arc<FakeCamera>` usable as `Arc<dyn Camera>`.
///
/// [`build`]: FakeCameraBuilder::build
#[derive(Default)]
pub struct FakeCameraBuilder {
	state: FakeState,
}

impl FakeCameraBuilder {
	/// Start a new builder with every method unconfigured.
	pub fn new() -> Self {
		Self::default()
	}

	/// Install the closure invoked on [`Camera::battery_status`].
	pub fn with_battery_status<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<BatteryStatus> + Send + Sync + 'static,
	{
		self.state.battery_status = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::pir_config`].
	pub fn with_pir_config<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<RfAlarmCfg> + Send + Sync + 'static,
	{
		self.state.pir_config = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on
	/// [`Camera::is_floodlight_tasks_enabled`].
	pub fn with_is_floodlight_tasks_enabled<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<bool> + Send + Sync + 'static,
	{
		self.state.is_floodlight_tasks_enabled = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::snapshot`].
	pub fn with_snapshot<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<Vec<u8>> + Send + Sync + 'static,
	{
		self.state.snapshot = Some(Box::new(f));
		self
	}

	/// Pre-seed the [`MotionData`] handle returned by
	/// [`Camera::listen_on_motion`]. Consumed on first call; second
	/// call panics.
	pub fn with_motion_stream(mut self, data: MotionData) -> Self {
		self.state.motion_stream = Mutex::new(Some(data));
		self
	}

	/// Pre-seed the floodlight receiver returned by
	/// [`Camera::listen_on_floodlight`]. Consumed on first call;
	/// second call panics.
	pub fn with_floodlight_stream(mut self, rx: Receiver<FloodlightStatusList>) -> Self {
		self.state.floodlight_stream = Mutex::new(Some(rx));
		self
	}

	/// Pre-seed the [`VideoStream`] returned by
	/// [`Camera::start_video`]. Consumed on first call; second call
	/// panics.
	pub fn with_video_stream(mut self, stream: Box<dyn VideoStream>) -> Self {
		self.state.video_stream = Mutex::new(Some(stream));
		self
	}

	/// Configure [`Camera::listen_on_motion`] to return `Err(f())`
	/// on every call. Used by retry-loop tests that need the listener to
	/// observe a subscribe failure without panicking.
	pub fn with_motion_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.state.motion_stream_error = Some(Box::new(f));
		self
	}

	/// Configure [`Camera::listen_on_floodlight`] to return
	/// `Err(f())` on every call. Used by retry-loop tests that need the
	/// listener to observe a subscribe failure without panicking.
	pub fn with_floodlight_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.state.floodlight_stream_error = Some(Box::new(f));
		self
	}

	/// Configure [`Camera::start_video`] to return `Err(f())` on
	/// every call.
	pub fn with_video_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.state.video_stream_error = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::led_state`].
	pub fn with_led_state<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<LedState> + Send + Sync + 'static,
	{
		self.state.led_state = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::ptz_presets`].
	pub fn with_ptz_presets<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<Vec<PresetSlot>> + Send + Sync + 'static,
	{
		self.state.ptz_presets = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::version`].
	pub fn with_version<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<VersionInfo> + Send + Sync + 'static,
	{
		self.state.version = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::users`].
	pub fn with_users<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<UserList> + Send + Sync + 'static,
	{
		self.state.users = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::service`] for every
	/// [`ServiceKind`].
	pub fn with_service<F>(mut self, f: F) -> Self
	where
		F: Fn(ServiceKind) -> CameraResult<ServicePortState> + Send + Sync + 'static,
	{
		self.state.service = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::keepalive_probe`].
	pub fn with_keepalive_probe<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<()> + Send + Sync + 'static,
	{
		self.state.keepalive_probe = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::capabilities`].
	pub fn with_capabilities<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<CameraCapabilities> + Send + Sync + 'static,
	{
		self.state.capabilities = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`Camera::ability_info`].
	pub fn with_ability_info<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<AbilityInfo> + Send + Sync + 'static,
	{
		self.state.ability_info = Some(Box::new(f));
		self
	}

	/// Make `battery_status` await `pending()` forever — used by 30 s
	/// command-timeout tests under `tokio::time::pause()`. Mutually
	/// exclusive with `with_battery_status(...)`; the pending flag
	/// short-circuits before the closure runs.
	pub fn with_battery_status_pending(mut self) -> Self {
		self.state.battery_status_pending = true;
		self
	}

	/// Make `pir_config` await `pending()` forever.
	pub fn with_pir_config_pending(mut self) -> Self {
		self.state.pir_config_pending = true;
		self
	}

	/// Make `snapshot` await `pending()` forever.
	pub fn with_snapshot_pending(mut self) -> Self {
		self.state.snapshot_pending = true;
		self
	}

	/// Make `send_ptz` await `pending()` forever — both directional and
	/// stop calls hang. The PTZ dispatcher's outer `timeout(30s)` pulls
	/// us out, so a paused virtual clock + `advance(31s)` is required.
	pub fn with_send_ptz_pending(mut self) -> Self {
		self.state.send_ptz_pending = true;
		self
	}

	/// Make `siren()` await `pending()` forever.
	pub fn with_siren_pending(mut self) -> Self {
		self.state.siren_pending = true;
		self
	}

	/// Make `is_floodlight_tasks_enabled()` await `pending()` forever.
	pub fn with_is_floodlight_tasks_enabled_pending(mut self) -> Self {
		self.state.is_floodlight_tasks_enabled_pending = true;
		self
	}

	/// Make `ptz_presets()` await `pending()` forever.
	pub fn with_ptz_presets_pending(mut self) -> Self {
		self.state.ptz_presets_pending = true;
		self
	}

	/// Finalise the fake and hand back a shared handle.
	pub fn build(self) -> Arc<FakeCamera> {
		Arc::new(FakeCamera {
			inner: Arc::new(self.state),
		})
	}
}

fn unset(method: &'static str) -> ! {
	panic!("FakeCamera: {} not configured for this test", method)
}

#[async_trait]
impl Camera for FakeCamera {
	async fn end_session(&self) -> CameraResult<()> {
		*self.inner.calls.end_session.lock().unwrap() += 1;
		Ok(())
	}

	async fn keepalive_probe(&self) -> CameraResult<()> {
		match self.inner.keepalive_probe.as_ref() {
			Some(f) => f(),
			None => unset("keepalive_probe"),
		}
	}

	async fn start_video(&self, kind: StreamKind) -> CameraResult<Box<dyn VideoStream>> {
		self.inner.calls.start_video.lock().unwrap().push(kind);
		if let Some(f) = self.inner.video_stream_error.as_ref() {
			return Err(f());
		}
		match self.inner.video_stream.lock().unwrap().take() {
			Some(stream) => Ok(stream),
			None => unset("start_video (stream not configured, or already consumed)"),
		}
	}

	async fn stop_video(&self, kind: StreamKind) -> CameraResult<()> {
		self.inner.calls.stop_video.lock().unwrap().push(kind);
		Ok(())
	}

	async fn listen_on_motion(&self) -> CameraResult<MotionData> {
		if let Some(f) = self.inner.motion_stream_error.as_ref() {
			return Err(f());
		}
		match self.inner.motion_stream.lock().unwrap().take() {
			Some(data) => Ok(data),
			None => unset("listen_on_motion (stream not configured, or already consumed)"),
		}
	}

	async fn listen_on_floodlight(&self) -> CameraResult<Receiver<FloodlightStatusList>> {
		if let Some(f) = self.inner.floodlight_stream_error.as_ref() {
			return Err(f());
		}
		match self.inner.floodlight_stream.lock().unwrap().take() {
			Some(rx) => Ok(rx),
			None => unset("listen_on_floodlight (stream not configured, or already consumed)"),
		}
	}

	async fn battery_status(&self) -> CameraResult<BatteryStatus> {
		if self.inner.battery_status_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.inner.battery_status.as_ref() {
			Some(f) => f(),
			None => unset("battery_status"),
		}
	}

	async fn pir_config(&self) -> CameraResult<RfAlarmCfg> {
		if self.inner.pir_config_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.inner.pir_config.as_ref() {
			Some(f) => f(),
			None => unset("pir_config"),
		}
	}

	async fn is_floodlight_tasks_enabled(&self) -> CameraResult<bool> {
		if self.inner.is_floodlight_tasks_enabled_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.inner.is_floodlight_tasks_enabled.as_ref() {
			Some(f) => f(),
			None => unset("is_floodlight_tasks_enabled"),
		}
	}

	async fn capabilities(&self) -> CameraResult<CameraCapabilities> {
		match self.inner.capabilities.as_ref() {
			Some(f) => f(),
			None => unset("capabilities"),
		}
	}

	async fn ptz_presets(&self) -> CameraResult<Vec<PresetSlot>> {
		if self.inner.ptz_presets_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.inner.ptz_presets.as_ref() {
			Some(f) => f(),
			None => unset("ptz_presets"),
		}
	}

	async fn version(&self) -> CameraResult<VersionInfo> {
		match self.inner.version.as_ref() {
			Some(f) => f(),
			None => unset("version"),
		}
	}

	async fn led_state(&self) -> CameraResult<LedState> {
		match self.inner.led_state.as_ref() {
			Some(f) => f(),
			None => unset("led_state"),
		}
	}

	async fn ability_info(&self) -> CameraResult<AbilityInfo> {
		match self.inner.ability_info.as_ref() {
			Some(f) => f(),
			None => unset("ability_info"),
		}
	}

	async fn snapshot(&self) -> CameraResult<Vec<u8>> {
		if self.inner.snapshot_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.inner.snapshot.as_ref() {
			Some(f) => f(),
			None => unset("snapshot"),
		}
	}

	async fn pir_set(&self, state: bool) -> CameraResult<()> {
		self.inner.calls.pir_set.lock().unwrap().push(state);
		Ok(())
	}

	async fn floodlight_tasks_enable(&self, state: bool) -> CameraResult<()> {
		self.inner
			.calls
			.floodlight_tasks_enable
			.lock()
			.unwrap()
			.push(state);
		Ok(())
	}

	async fn set_floodlight_manual(&self, state: bool, duration: u16) -> CameraResult<()> {
		self.inner
			.calls
			.set_floodlight_manual
			.lock()
			.unwrap()
			.push((state, duration));
		Ok(())
	}

	async fn send_ptz(&self, direction: Direction, amount: f32) -> CameraResult<()> {
		self.inner
			.calls
			.send_ptz
			.lock()
			.unwrap()
			.push((direction, amount));
		if self.inner.send_ptz_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		Ok(())
	}

	async fn set_ptz_preset(&self, preset_id: u8, name: String) -> CameraResult<()> {
		self.inner
			.calls
			.set_ptz_preset
			.lock()
			.unwrap()
			.push((preset_id, name));
		Ok(())
	}

	async fn moveto_ptz_preset(&self, preset_id: u8) -> CameraResult<()> {
		self.inner
			.calls
			.moveto_ptz_preset
			.lock()
			.unwrap()
			.push(preset_id);
		Ok(())
	}

	async fn zoom_to(&self, level: ZoomLevel) -> CameraResult<()> {
		self.inner.calls.zoom_to.lock().unwrap().push(level);
		Ok(())
	}

	async fn led_light_set(&self, state: bool) -> CameraResult<()> {
		self.inner.calls.led_light_set.lock().unwrap().push(state);
		Ok(())
	}

	async fn irled_light_set(&self, state: LightState) -> CameraResult<()> {
		self.inner.calls.irled_light_set.lock().unwrap().push(state);
		Ok(())
	}

	async fn reboot(&self) -> CameraResult<()> {
		*self.inner.calls.reboot.lock().unwrap() += 1;
		Ok(())
	}

	async fn siren(&self) -> CameraResult<()> {
		*self.inner.calls.siren.lock().unwrap() += 1;
		if self.inner.siren_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		Ok(())
	}

	async fn set_time(&self, timestamp: OffsetDateTime) -> CameraResult<()> {
		self.inner.calls.set_time.lock().unwrap().push(timestamp);
		Ok(())
	}

	async fn users(&self) -> CameraResult<UserList> {
		match self.inner.users.as_ref() {
			Some(f) => f(),
			None => unset("users"),
		}
	}

	async fn add_user(
		&self,
		user_name: String,
		password: String,
		user_level: u8,
	) -> CameraResult<()> {
		self.inner
			.calls
			.add_user
			.lock()
			.unwrap()
			.push((user_name, password, user_level));
		Ok(())
	}

	async fn modify_user(&self, user_name: String, password: String) -> CameraResult<()> {
		self.inner
			.calls
			.modify_user
			.lock()
			.unwrap()
			.push((user_name, password));
		Ok(())
	}

	async fn delete_user(&self, user_name: String) -> CameraResult<()> {
		self.inner.calls.delete_user.lock().unwrap().push(user_name);
		Ok(())
	}

	async fn service(&self, kind: ServiceKind) -> CameraResult<ServicePortState> {
		match self.inner.service.as_ref() {
			Some(f) => f(kind),
			None => unset("service"),
		}
	}

	async fn set_service(
		&self,
		kind: ServiceKind,
		enable: Option<bool>,
		port: Option<u32>,
	) -> CameraResult<()> {
		self.inner
			.calls
			.set_service
			.lock()
			.unwrap()
			.push((kind, enable, port));
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::battery::Millivolts;

	/// End-to-end smoke test: a builder-scripted read value and a
	/// recorded setter call both come back through the trait object.
	#[tokio::test]
	async fn fake_camera_scaffolding_compiles_and_drives_through_trait() {
		let fake = FakeCameraBuilder::new()
			.with_battery_status(|| {
				Ok(BatteryStatus {
					percent: 42,
					voltage: Millivolts(3985),
					charge_status: "charging".into(),
					low_power: false,
				})
			})
			.with_snapshot(|| Ok(b"jpeg-bytes".to_vec()))
			.build();

		// Hold as Arc<dyn Camera> to prove dyn-compatibility.
		let driver: Arc<dyn Camera> = fake.clone();

		let status = driver.battery_status().await.unwrap();
		assert_eq!(status.percent, 42);
		assert_eq!(status.voltage, Millivolts(3985));

		let snap = driver.snapshot().await.unwrap();
		assert_eq!(snap, b"jpeg-bytes");

		driver.pir_set(true).await.unwrap();
		driver.reboot().await.unwrap();
		driver.reboot().await.unwrap();
		driver.end_session().await.unwrap();

		assert_eq!(*fake.calls().pir_set.lock().unwrap(), vec![true]);
		assert_eq!(*fake.calls().reboot.lock().unwrap(), 2);
		assert_eq!(*fake.calls().end_session.lock().unwrap(), 1);
	}

	#[tokio::test]
	#[should_panic(expected = "FakeCamera: battery_status not configured")]
	async fn unconfigured_read_method_panics_with_clear_message() {
		let fake = FakeCameraBuilder::new().build();
		let _ = fake.battery_status().await;
	}

	#[tokio::test]
	async fn service_closure_keys_all_kinds_and_set_service_records() {
		let fake = FakeCameraBuilder::new()
			.with_service(|kind| {
				Ok(ServicePortState {
					port: match kind {
						ServiceKind::Http => 80,
						_ => 9000,
					},
					enabled: Some(true),
				})
			})
			.build();
		let driver: Arc<dyn Camera> = fake.clone();
		assert_eq!(driver.service(ServiceKind::Http).await.unwrap().port, 80);
		assert_eq!(
			driver.service(ServiceKind::Baichuan).await.unwrap().port,
			9000
		);
		driver
			.set_service(ServiceKind::Rtsp, Some(true), Some(8554))
			.await
			.unwrap();
		assert_eq!(
			*fake.calls().set_service.lock().unwrap(),
			vec![(ServiceKind::Rtsp, Some(true), Some(8554))]
		);
	}
}
