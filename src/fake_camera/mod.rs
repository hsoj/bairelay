//! A scripted stand-in for a real camera.
//!
//! Mirrors production's composition: [`roles`] holds one small fake per
//! `crate::camera` role trait, and [`FakeCamera`] composes all eight
//! behind the blanket `Camera` impl — the same shape `BcCamera` has on
//! the production side. Tests for a narrow consumer build just the role
//! fake they need; tests that wire a whole session use
//! [`FakeCameraBuilder`], whose surface is unchanged from the
//! pre-split single-struct fake.
//!
//! Backed by closures the test sets per method. Unset read/stream
//! methods panic with a clear diagnostic so tests fail fast rather than
//! silently returning bogus data. Side-effect setters (`pir_set`,
//! `reboot`, …) record their arguments and invocation counts in
//! [`FakeCalls`] for assertion.
//!
//! `#[cfg(test)]`-gated in `lib.rs`, so production builds cannot link
//! it and substitute a fake for a real camera (`TS-2`).

pub mod roles;

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
use crate::camera::{
	Camera, CameraResult, DeviceAdmin, Events, Lighting, Power, Ptz, Session, Stills, Video,
};
use crate::camera_services::{ServiceKind, ServicePortState};
use crate::capabilities::CameraCapabilities;
use crate::ptz::{PresetSlot, ZoomLevel};

pub use roles::{
	FakeDeviceAdmin, FakeEvents, FakeLighting, FakePower, FakePtz, FakeSession, FakeStills,
	FakeVideo,
};

pub(crate) type BoxFn<T> = Box<dyn Fn() -> CameraResult<T> + Send + Sync>;

/// One recorded `set_service` call: which service, and the enable /
/// port fields the caller asked to change (`None` = leave alone).
type SetServiceCall = (ServiceKind, Option<bool>, Option<u32>);

/// Call log populated by side-effect methods on the fakes. Tests read
/// these fields after exercising the code under test to confirm the
/// right camera calls were made with the right arguments.
///
/// One flat ledger shared by all eight role fakes rather than a ledger
/// per role: field names are unique across roles, and the existing
/// assertions address fields directly (`calls().pir_set`), so a
/// per-role split would churn every assertion without changing what
/// any test proves.
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

pub(crate) fn unset(method: &'static str) -> ! {
	panic!("FakeCamera: {} not configured for this test", method)
}

/// All eight role fakes composed into one full camera, driven entirely
/// by closures and pre-built stream values configured on the
/// [`FakeCameraBuilder`]. The blanket impl in `crate::camera` supplies
/// `Camera`, exactly as it does for `BcCamera`.
///
/// Access the call log via [`FakeCamera::calls`] after the code under
/// test has run. An unconfigured read or stream method panics with a
/// `"FakeCamera: <method> not configured for this test"` message —
/// silent defaults are deliberately not supported because they hide
/// test-setup bugs.
pub struct FakeCamera {
	session: FakeSession,
	video: FakeVideo,
	stills: FakeStills,
	events: FakeEvents,
	power: FakePower,
	lighting: FakeLighting,
	ptz: FakePtz,
	admin: FakeDeviceAdmin,
	calls: Arc<FakeCalls>,
}

impl FakeCamera {
	/// Access the call log recorded by side-effect methods.
	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}
}

#[async_trait]
impl Session for FakeCamera {
	async fn end_session(&self) -> CameraResult<()> {
		self.session.end_session().await
	}

	async fn keepalive_probe(&self) -> CameraResult<()> {
		self.session.keepalive_probe().await
	}
}

#[async_trait]
impl Video for FakeCamera {
	async fn start_video(&self, kind: StreamKind) -> CameraResult<Box<dyn VideoStream>> {
		self.video.start_video(kind).await
	}

	async fn stop_video(&self, kind: StreamKind) -> CameraResult<()> {
		self.video.stop_video(kind).await
	}
}

#[async_trait]
impl Stills for FakeCamera {
	async fn snapshot(&self) -> CameraResult<Vec<u8>> {
		self.stills.snapshot().await
	}
}

#[async_trait]
impl Events for FakeCamera {
	async fn listen_on_motion(&self) -> CameraResult<MotionData> {
		self.events.listen_on_motion().await
	}

	async fn listen_on_floodlight(&self) -> CameraResult<Receiver<FloodlightStatusList>> {
		self.events.listen_on_floodlight().await
	}
}

#[async_trait]
impl Power for FakeCamera {
	async fn battery_status(&self) -> CameraResult<BatteryStatus> {
		self.power.battery_status().await
	}

	async fn pir_config(&self) -> CameraResult<RfAlarmCfg> {
		self.power.pir_config().await
	}

	async fn pir_set(&self, state: bool) -> CameraResult<()> {
		self.power.pir_set(state).await
	}
}

#[async_trait]
impl Lighting for FakeCamera {
	async fn led_state(&self) -> CameraResult<LedState> {
		self.lighting.led_state().await
	}

	async fn led_light_set(&self, state: bool) -> CameraResult<()> {
		self.lighting.led_light_set(state).await
	}

	async fn irled_light_set(&self, state: LightState) -> CameraResult<()> {
		self.lighting.irled_light_set(state).await
	}

	async fn is_floodlight_tasks_enabled(&self) -> CameraResult<bool> {
		self.lighting.is_floodlight_tasks_enabled().await
	}

	async fn floodlight_tasks_enable(&self, state: bool) -> CameraResult<()> {
		self.lighting.floodlight_tasks_enable(state).await
	}

	async fn set_floodlight_manual(&self, state: bool, duration: u16) -> CameraResult<()> {
		self.lighting.set_floodlight_manual(state, duration).await
	}

	async fn siren(&self) -> CameraResult<()> {
		self.lighting.siren().await
	}
}

#[async_trait]
impl Ptz for FakeCamera {
	async fn send_ptz(&self, direction: Direction, amount: f32) -> CameraResult<()> {
		self.ptz.send_ptz(direction, amount).await
	}

	async fn ptz_presets(&self) -> CameraResult<Vec<PresetSlot>> {
		self.ptz.ptz_presets().await
	}

	async fn set_ptz_preset(&self, preset_id: u8, name: String) -> CameraResult<()> {
		self.ptz.set_ptz_preset(preset_id, name).await
	}

	async fn moveto_ptz_preset(&self, preset_id: u8) -> CameraResult<()> {
		self.ptz.moveto_ptz_preset(preset_id).await
	}

	async fn zoom_to(&self, level: ZoomLevel) -> CameraResult<()> {
		self.ptz.zoom_to(level).await
	}
}

#[async_trait]
impl DeviceAdmin for FakeCamera {
	async fn capabilities(&self) -> CameraResult<CameraCapabilities> {
		self.admin.capabilities().await
	}

	async fn version(&self) -> CameraResult<VersionInfo> {
		self.admin.version().await
	}

	async fn ability_info(&self) -> CameraResult<AbilityInfo> {
		self.admin.ability_info().await
	}

	async fn reboot(&self) -> CameraResult<()> {
		self.admin.reboot().await
	}

	async fn set_time(&self, timestamp: OffsetDateTime) -> CameraResult<()> {
		self.admin.set_time(timestamp).await
	}

	async fn users(&self) -> CameraResult<UserList> {
		self.admin.users().await
	}

	async fn add_user(
		&self,
		user_name: String,
		password: String,
		user_level: u8,
	) -> CameraResult<()> {
		self.admin.add_user(user_name, password, user_level).await
	}

	async fn modify_user(&self, user_name: String, password: String) -> CameraResult<()> {
		self.admin.modify_user(user_name, password).await
	}

	async fn delete_user(&self, user_name: String) -> CameraResult<()> {
		self.admin.delete_user(user_name).await
	}

	async fn service(&self, kind: ServiceKind) -> CameraResult<ServicePortState> {
		self.admin.service(kind).await
	}

	async fn set_service(
		&self,
		kind: ServiceKind,
		enable: Option<bool>,
		port: Option<u32>,
	) -> CameraResult<()> {
		self.admin.set_service(kind, enable, port).await
	}
}

/// Builder for a [`FakeCamera`]. Use `with_<method>(...)` to install a
/// closure per read method, or `with_<method>_stream(...)` to install
/// a pre-built value for a stream-returning method. Call [`build`] to
/// produce an `Arc<FakeCamera>` usable as `Arc<dyn Camera>`.
///
/// Every setter delegates to the per-role fake in [`roles`]; the
/// builder exists so whole-session tests configure one object instead
/// of eight.
///
/// [`build`]: FakeCameraBuilder::build
pub struct FakeCameraBuilder {
	session: FakeSession,
	video: FakeVideo,
	stills: FakeStills,
	events: FakeEvents,
	power: FakePower,
	lighting: FakeLighting,
	ptz: FakePtz,
	admin: FakeDeviceAdmin,
	calls: Arc<FakeCalls>,
}

impl Default for FakeCameraBuilder {
	fn default() -> Self {
		Self::new()
	}
}

impl FakeCameraBuilder {
	/// Start a new builder with every method unconfigured. All eight
	/// role fakes share one [`FakeCalls`] ledger.
	pub fn new() -> Self {
		let calls = Arc::new(FakeCalls::default());
		Self {
			session: FakeSession::with_ledger(Arc::clone(&calls)),
			video: FakeVideo::with_ledger(Arc::clone(&calls)),
			stills: FakeStills::with_ledger(Arc::clone(&calls)),
			events: FakeEvents::with_ledger(Arc::clone(&calls)),
			power: FakePower::with_ledger(Arc::clone(&calls)),
			lighting: FakeLighting::with_ledger(Arc::clone(&calls)),
			ptz: FakePtz::with_ledger(Arc::clone(&calls)),
			admin: FakeDeviceAdmin::with_ledger(Arc::clone(&calls)),
			calls,
		}
	}

	/// Install the closure invoked on [`Power::battery_status`].
	pub fn with_battery_status<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<BatteryStatus> + Send + Sync + 'static,
	{
		self.power = self.power.with_battery_status(f);
		self
	}

	/// Install the closure invoked on [`Power::pir_config`].
	pub fn with_pir_config<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<RfAlarmCfg> + Send + Sync + 'static,
	{
		self.power = self.power.with_pir_config(f);
		self
	}

	/// Install the closure invoked on
	/// [`Lighting::is_floodlight_tasks_enabled`].
	pub fn with_is_floodlight_tasks_enabled<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<bool> + Send + Sync + 'static,
	{
		self.lighting = self.lighting.with_is_floodlight_tasks_enabled(f);
		self
	}

	/// Install the closure invoked on [`Stills::snapshot`].
	pub fn with_snapshot<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<Vec<u8>> + Send + Sync + 'static,
	{
		self.stills = self.stills.with_snapshot(f);
		self
	}

	/// Pre-seed the [`MotionData`] handle returned by
	/// [`Events::listen_on_motion`]. Consumed on first call; second
	/// call panics.
	pub fn with_motion_stream(mut self, data: MotionData) -> Self {
		self.events = self.events.with_motion_stream(data);
		self
	}

	/// Pre-seed the floodlight receiver returned by
	/// [`Events::listen_on_floodlight`]. Consumed on first call;
	/// second call panics.
	pub fn with_floodlight_stream(mut self, rx: Receiver<FloodlightStatusList>) -> Self {
		self.events = self.events.with_floodlight_stream(rx);
		self
	}

	/// Pre-seed the [`VideoStream`] returned by
	/// [`Video::start_video`]. Consumed on first call; second call
	/// panics.
	pub fn with_video_stream(mut self, stream: Box<dyn VideoStream>) -> Self {
		self.video = self.video.with_video_stream(stream);
		self
	}

	/// Configure [`Events::listen_on_motion`] to return `Err(f())`
	/// on every call. Used by retry-loop tests that need the listener to
	/// observe a subscribe failure without panicking.
	pub fn with_motion_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.events = self.events.with_motion_stream_error(f);
		self
	}

	/// Configure [`Events::listen_on_floodlight`] to return
	/// `Err(f())` on every call. Used by retry-loop tests that need the
	/// listener to observe a subscribe failure without panicking.
	pub fn with_floodlight_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.events = self.events.with_floodlight_stream_error(f);
		self
	}

	/// Configure [`Video::start_video`] to return `Err(f())` on
	/// every call.
	pub fn with_video_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.video = self.video.with_video_stream_error(f);
		self
	}

	/// Install the closure invoked on [`Lighting::led_state`].
	pub fn with_led_state<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<LedState> + Send + Sync + 'static,
	{
		self.lighting = self.lighting.with_led_state(f);
		self
	}

	/// Install the closure invoked on [`Ptz::ptz_presets`].
	pub fn with_ptz_presets<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<Vec<PresetSlot>> + Send + Sync + 'static,
	{
		self.ptz = self.ptz.with_ptz_presets(f);
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::version`].
	pub fn with_version<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<VersionInfo> + Send + Sync + 'static,
	{
		self.admin = self.admin.with_version(f);
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::users`].
	pub fn with_users<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<UserList> + Send + Sync + 'static,
	{
		self.admin = self.admin.with_users(f);
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::service`] for every
	/// [`ServiceKind`].
	pub fn with_service<F>(mut self, f: F) -> Self
	where
		F: Fn(ServiceKind) -> CameraResult<ServicePortState> + Send + Sync + 'static,
	{
		self.admin = self.admin.with_service(f);
		self
	}

	/// Install the closure invoked on [`Session::keepalive_probe`].
	pub fn with_keepalive_probe<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<()> + Send + Sync + 'static,
	{
		self.session = self.session.with_keepalive_probe(f);
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::capabilities`].
	pub fn with_capabilities<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<CameraCapabilities> + Send + Sync + 'static,
	{
		self.admin = self.admin.with_capabilities(f);
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::ability_info`].
	pub fn with_ability_info<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<AbilityInfo> + Send + Sync + 'static,
	{
		self.admin = self.admin.with_ability_info(f);
		self
	}

	/// Make `battery_status` await `pending()` forever — used by 30 s
	/// command-timeout tests under `tokio::time::pause()`. Mutually
	/// exclusive with `with_battery_status(...)`; the pending flag
	/// short-circuits before the closure runs.
	pub fn with_battery_status_pending(mut self) -> Self {
		self.power = self.power.with_battery_status_pending();
		self
	}

	/// Make `pir_config` await `pending()` forever.
	pub fn with_pir_config_pending(mut self) -> Self {
		self.power = self.power.with_pir_config_pending();
		self
	}

	/// Make `snapshot` await `pending()` forever.
	pub fn with_snapshot_pending(mut self) -> Self {
		self.stills = self.stills.with_snapshot_pending();
		self
	}

	/// Make `send_ptz` await `pending()` forever — both directional and
	/// stop calls hang. The PTZ dispatcher's outer `timeout(30s)` pulls
	/// us out, so a paused virtual clock + `advance(31s)` is required.
	pub fn with_send_ptz_pending(mut self) -> Self {
		self.ptz = self.ptz.with_send_ptz_pending();
		self
	}

	/// Make `siren()` await `pending()` forever.
	pub fn with_siren_pending(mut self) -> Self {
		self.lighting = self.lighting.with_siren_pending();
		self
	}

	/// Make `is_floodlight_tasks_enabled()` await `pending()` forever.
	pub fn with_is_floodlight_tasks_enabled_pending(mut self) -> Self {
		self.lighting = self.lighting.with_is_floodlight_tasks_enabled_pending();
		self
	}

	/// Make `ptz_presets()` await `pending()` forever.
	pub fn with_ptz_presets_pending(mut self) -> Self {
		self.ptz = self.ptz.with_ptz_presets_pending();
		self
	}

	/// Finalise the fake and hand back a shared handle.
	pub fn build(self) -> Arc<FakeCamera> {
		Arc::new(FakeCamera {
			session: self.session,
			video: self.video,
			stills: self.stills,
			events: self.events,
			power: self.power,
			lighting: self.lighting,
			ptz: self.ptz,
			admin: self.admin,
			calls: self.calls,
		})
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

	/// A per-role fake satisfies exactly its role bound and records
	/// into its own ledger — the seam narrow-consumer tests build on.
	#[tokio::test]
	async fn standalone_role_fake_drives_through_narrow_trait_object() {
		let fake = FakePower::new()
			.with_battery_status(|| {
				Ok(BatteryStatus {
					percent: 7,
					voltage: Millivolts(3600),
					charge_status: "discharging".into(),
					low_power: true,
				})
			})
			.build();

		let power: Arc<dyn Power> = fake.clone();
		assert_eq!(power.battery_status().await.unwrap().percent, 7);

		power.pir_set(false).await.unwrap();
		assert_eq!(*fake.calls().pir_set.lock().unwrap(), vec![false]);
	}
}
