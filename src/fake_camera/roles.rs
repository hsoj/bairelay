//! One scripted fake per camera role trait.
//!
//! Each fake implements exactly one `crate::camera` role, so a test for
//! a narrow consumer (`battery_poller` takes `Arc<dyn Power>`) can
//! build only the role it exercises — and if the consumer's bound ever
//! widens, the test stops compiling instead of silently passing.
//! [`super::FakeCamera`] composes all eight for tests that need a full
//! `Arc<dyn Camera>`.
//!
//! All fakes record side-effect calls into the shared [`FakeCalls`]
//! ledger (one flat struct, unique field names) rather than per-role
//! ledgers: the 59 existing `calls()` assertions address fields
//! directly, and splitting the ledger would churn them for no added
//! proof value.

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
	CameraResult, DeviceAdmin, Events, Lighting, Power, Ptz, Session, Stills, Video,
};
use crate::camera_services::{ServiceKind, ServicePortState};
use crate::capabilities::CameraCapabilities;
use crate::ptz::{PresetSlot, ZoomLevel};

use super::{unset, BoxFn, FakeCalls};

/// Scripted [`Session`] role: `end_session` records, `keepalive_probe`
/// runs its configured closure.
pub struct FakeSession {
	keepalive_probe: Option<BoxFn<()>>,
	calls: Arc<FakeCalls>,
}

impl FakeSession {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			keepalive_probe: None,
			calls,
		}
	}

	/// Install the closure invoked on [`Session::keepalive_probe`].
	pub fn with_keepalive_probe<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<()> + Send + Sync + 'static,
	{
		self.keepalive_probe = Some(Box::new(f));
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakeSession {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl Session for FakeSession {
	async fn end_session(&self) -> CameraResult<()> {
		*self.calls.end_session.lock().unwrap() += 1;
		Ok(())
	}

	async fn keepalive_probe(&self) -> CameraResult<()> {
		match self.keepalive_probe.as_ref() {
			Some(f) => f(),
			None => unset("keepalive_probe"),
		}
	}
}

/// Scripted [`Video`] role: a pre-built stream consumed on first
/// `start_video`, or a scripted error on every call.
pub struct FakeVideo {
	video_stream: Mutex<Option<Box<dyn VideoStream>>>,
	video_stream_error: Option<Box<dyn Fn() -> Error + Send + Sync>>,
	calls: Arc<FakeCalls>,
}

impl FakeVideo {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			video_stream: Mutex::new(None),
			video_stream_error: None,
			calls,
		}
	}

	/// Pre-seed the [`VideoStream`] returned by [`Video::start_video`].
	/// Consumed on first call; second call panics.
	pub fn with_video_stream(self, stream: Box<dyn VideoStream>) -> Self {
		*self.video_stream.lock().unwrap() = Some(stream);
		self
	}

	/// Configure [`Video::start_video`] to return `Err(f())` on every
	/// call.
	pub fn with_video_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.video_stream_error = Some(Box::new(f));
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakeVideo {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl Video for FakeVideo {
	async fn start_video(&self, kind: StreamKind) -> CameraResult<Box<dyn VideoStream>> {
		self.calls.start_video.lock().unwrap().push(kind);
		if let Some(f) = self.video_stream_error.as_ref() {
			return Err(f());
		}
		match self.video_stream.lock().unwrap().take() {
			Some(stream) => Ok(stream),
			None => unset("start_video (stream not configured, or already consumed)"),
		}
	}

	async fn stop_video(&self, kind: StreamKind) -> CameraResult<()> {
		self.calls.stop_video.lock().unwrap().push(kind);
		Ok(())
	}
}

/// Scripted [`Stills`] role.
pub struct FakeStills {
	snapshot: Option<BoxFn<Vec<u8>>>,
	snapshot_pending: bool,
	calls: Arc<FakeCalls>,
}

impl FakeStills {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			snapshot: None,
			snapshot_pending: false,
			calls,
		}
	}

	/// Install the closure invoked on [`Stills::snapshot`].
	pub fn with_snapshot<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<Vec<u8>> + Send + Sync + 'static,
	{
		self.snapshot = Some(Box::new(f));
		self
	}

	/// Make `snapshot` await `pending()` forever.
	pub fn with_snapshot_pending(mut self) -> Self {
		self.snapshot_pending = true;
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakeStills {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl Stills for FakeStills {
	async fn snapshot(&self) -> CameraResult<Vec<u8>> {
		if self.snapshot_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.snapshot.as_ref() {
			Some(f) => f(),
			None => unset("snapshot"),
		}
	}
}

/// Scripted [`Events`] role: pre-built subscription values consumed on
/// first call, or scripted errors on every call.
pub struct FakeEvents {
	motion_stream: Mutex<Option<MotionData>>,
	motion_stream_error: Option<Box<dyn Fn() -> Error + Send + Sync>>,
	floodlight_stream: Mutex<Option<Receiver<FloodlightStatusList>>>,
	floodlight_stream_error: Option<Box<dyn Fn() -> Error + Send + Sync>>,
	calls: Arc<FakeCalls>,
}

impl FakeEvents {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			motion_stream: Mutex::new(None),
			motion_stream_error: None,
			floodlight_stream: Mutex::new(None),
			floodlight_stream_error: None,
			calls,
		}
	}

	/// Pre-seed the [`MotionData`] handle returned by
	/// [`Events::listen_on_motion`]. Consumed on first call; second
	/// call panics.
	pub fn with_motion_stream(self, data: MotionData) -> Self {
		*self.motion_stream.lock().unwrap() = Some(data);
		self
	}

	/// Configure [`Events::listen_on_motion`] to return `Err(f())` on
	/// every call. Used by retry-loop tests that need the listener to
	/// observe a subscribe failure without panicking.
	pub fn with_motion_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.motion_stream_error = Some(Box::new(f));
		self
	}

	/// Pre-seed the floodlight receiver returned by
	/// [`Events::listen_on_floodlight`]. Consumed on first call;
	/// second call panics.
	pub fn with_floodlight_stream(self, rx: Receiver<FloodlightStatusList>) -> Self {
		*self.floodlight_stream.lock().unwrap() = Some(rx);
		self
	}

	/// Configure [`Events::listen_on_floodlight`] to return `Err(f())`
	/// on every call.
	pub fn with_floodlight_stream_error<F>(mut self, f: F) -> Self
	where
		F: Fn() -> Error + Send + Sync + 'static,
	{
		self.floodlight_stream_error = Some(Box::new(f));
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakeEvents {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl Events for FakeEvents {
	async fn listen_on_motion(&self) -> CameraResult<MotionData> {
		if let Some(f) = self.motion_stream_error.as_ref() {
			return Err(f());
		}
		match self.motion_stream.lock().unwrap().take() {
			Some(data) => Ok(data),
			None => unset("listen_on_motion (stream not configured, or already consumed)"),
		}
	}

	async fn listen_on_floodlight(&self) -> CameraResult<Receiver<FloodlightStatusList>> {
		if let Some(f) = self.floodlight_stream_error.as_ref() {
			return Err(f());
		}
		match self.floodlight_stream.lock().unwrap().take() {
			Some(rx) => Ok(rx),
			None => unset("listen_on_floodlight (stream not configured, or already consumed)"),
		}
	}
}

/// Scripted [`Power`] role.
pub struct FakePower {
	battery_status: Option<BoxFn<BatteryStatus>>,
	battery_status_pending: bool,
	pir_config: Option<BoxFn<RfAlarmCfg>>,
	pir_config_pending: bool,
	calls: Arc<FakeCalls>,
}

impl FakePower {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			battery_status: None,
			battery_status_pending: false,
			pir_config: None,
			pir_config_pending: false,
			calls,
		}
	}

	/// Install the closure invoked on [`Power::battery_status`].
	pub fn with_battery_status<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<BatteryStatus> + Send + Sync + 'static,
	{
		self.battery_status = Some(Box::new(f));
		self
	}

	/// Make `battery_status` await `pending()` forever — used by 30 s
	/// command-timeout tests under `tokio::time::pause()`. Mutually
	/// exclusive with `with_battery_status(...)`; the pending flag
	/// short-circuits before the closure runs.
	pub fn with_battery_status_pending(mut self) -> Self {
		self.battery_status_pending = true;
		self
	}

	/// Install the closure invoked on [`Power::pir_config`].
	pub fn with_pir_config<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<RfAlarmCfg> + Send + Sync + 'static,
	{
		self.pir_config = Some(Box::new(f));
		self
	}

	/// Make `pir_config` await `pending()` forever.
	pub fn with_pir_config_pending(mut self) -> Self {
		self.pir_config_pending = true;
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakePower {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl Power for FakePower {
	async fn battery_status(&self) -> CameraResult<BatteryStatus> {
		if self.battery_status_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.battery_status.as_ref() {
			Some(f) => f(),
			None => unset("battery_status"),
		}
	}

	async fn pir_config(&self) -> CameraResult<RfAlarmCfg> {
		if self.pir_config_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.pir_config.as_ref() {
			Some(f) => f(),
			None => unset("pir_config"),
		}
	}

	async fn pir_set(&self, state: bool) -> CameraResult<()> {
		self.calls.pir_set.lock().unwrap().push(state);
		Ok(())
	}
}

/// Scripted [`Lighting`] role.
pub struct FakeLighting {
	led_state: Option<BoxFn<LedState>>,
	is_floodlight_tasks_enabled: Option<BoxFn<bool>>,
	is_floodlight_tasks_enabled_pending: bool,
	siren_pending: bool,
	calls: Arc<FakeCalls>,
}

impl FakeLighting {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			led_state: None,
			is_floodlight_tasks_enabled: None,
			is_floodlight_tasks_enabled_pending: false,
			siren_pending: false,
			calls,
		}
	}

	/// Install the closure invoked on [`Lighting::led_state`].
	pub fn with_led_state<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<LedState> + Send + Sync + 'static,
	{
		self.led_state = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on
	/// [`Lighting::is_floodlight_tasks_enabled`].
	pub fn with_is_floodlight_tasks_enabled<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<bool> + Send + Sync + 'static,
	{
		self.is_floodlight_tasks_enabled = Some(Box::new(f));
		self
	}

	/// Make `is_floodlight_tasks_enabled()` await `pending()` forever.
	pub fn with_is_floodlight_tasks_enabled_pending(mut self) -> Self {
		self.is_floodlight_tasks_enabled_pending = true;
		self
	}

	/// Make `siren()` await `pending()` forever.
	pub fn with_siren_pending(mut self) -> Self {
		self.siren_pending = true;
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakeLighting {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl Lighting for FakeLighting {
	async fn led_state(&self) -> CameraResult<LedState> {
		match self.led_state.as_ref() {
			Some(f) => f(),
			None => unset("led_state"),
		}
	}

	async fn led_light_set(&self, state: bool) -> CameraResult<()> {
		self.calls.led_light_set.lock().unwrap().push(state);
		Ok(())
	}

	async fn irled_light_set(&self, state: LightState) -> CameraResult<()> {
		self.calls.irled_light_set.lock().unwrap().push(state);
		Ok(())
	}

	async fn is_floodlight_tasks_enabled(&self) -> CameraResult<bool> {
		if self.is_floodlight_tasks_enabled_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.is_floodlight_tasks_enabled.as_ref() {
			Some(f) => f(),
			None => unset("is_floodlight_tasks_enabled"),
		}
	}

	async fn floodlight_tasks_enable(&self, state: bool) -> CameraResult<()> {
		self.calls
			.floodlight_tasks_enable
			.lock()
			.unwrap()
			.push(state);
		Ok(())
	}

	async fn set_floodlight_manual(&self, state: bool, duration: u16) -> CameraResult<()> {
		self.calls
			.set_floodlight_manual
			.lock()
			.unwrap()
			.push((state, duration));
		Ok(())
	}

	async fn siren(&self) -> CameraResult<()> {
		*self.calls.siren.lock().unwrap() += 1;
		if self.siren_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		Ok(())
	}
}

/// Scripted [`Ptz`] role.
pub struct FakePtz {
	ptz_presets: Option<BoxFn<Vec<PresetSlot>>>,
	ptz_presets_pending: bool,
	send_ptz_pending: bool,
	calls: Arc<FakeCalls>,
}

impl FakePtz {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			ptz_presets: None,
			ptz_presets_pending: false,
			send_ptz_pending: false,
			calls,
		}
	}

	/// Install the closure invoked on [`Ptz::ptz_presets`].
	pub fn with_ptz_presets<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<Vec<PresetSlot>> + Send + Sync + 'static,
	{
		self.ptz_presets = Some(Box::new(f));
		self
	}

	/// Make `ptz_presets()` await `pending()` forever.
	pub fn with_ptz_presets_pending(mut self) -> Self {
		self.ptz_presets_pending = true;
		self
	}

	/// Make `send_ptz` await `pending()` forever — both directional and
	/// stop calls hang. The PTZ dispatcher's outer `timeout(30s)` pulls
	/// us out, so a paused virtual clock + `advance(31s)` is required.
	pub fn with_send_ptz_pending(mut self) -> Self {
		self.send_ptz_pending = true;
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakePtz {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl Ptz for FakePtz {
	async fn send_ptz(&self, direction: Direction, amount: f32) -> CameraResult<()> {
		self.calls
			.send_ptz
			.lock()
			.unwrap()
			.push((direction, amount));
		if self.send_ptz_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		Ok(())
	}

	async fn ptz_presets(&self) -> CameraResult<Vec<PresetSlot>> {
		if self.ptz_presets_pending {
			std::future::pending::<()>().await;
			unreachable!()
		}
		match self.ptz_presets.as_ref() {
			Some(f) => f(),
			None => unset("ptz_presets"),
		}
	}

	async fn set_ptz_preset(&self, preset_id: u8, name: String) -> CameraResult<()> {
		self.calls
			.set_ptz_preset
			.lock()
			.unwrap()
			.push((preset_id, name));
		Ok(())
	}

	async fn moveto_ptz_preset(&self, preset_id: u8) -> CameraResult<()> {
		self.calls.moveto_ptz_preset.lock().unwrap().push(preset_id);
		Ok(())
	}

	async fn zoom_to(&self, level: ZoomLevel) -> CameraResult<()> {
		self.calls.zoom_to.lock().unwrap().push(level);
		Ok(())
	}
}

/// Scripted [`DeviceAdmin`] role.
pub struct FakeDeviceAdmin {
	capabilities: Option<BoxFn<CameraCapabilities>>,
	version: Option<BoxFn<VersionInfo>>,
	ability_info: Option<BoxFn<AbilityInfo>>,
	users: Option<BoxFn<UserList>>,
	service: Option<Box<dyn Fn(ServiceKind) -> CameraResult<ServicePortState> + Send + Sync>>,
	calls: Arc<FakeCalls>,
}

impl FakeDeviceAdmin {
	pub fn new() -> Self {
		Self::with_ledger(Arc::new(FakeCalls::default()))
	}

	pub(super) fn with_ledger(calls: Arc<FakeCalls>) -> Self {
		Self {
			capabilities: None,
			version: None,
			ability_info: None,
			users: None,
			service: None,
			calls,
		}
	}

	/// Install the closure invoked on [`DeviceAdmin::capabilities`].
	pub fn with_capabilities<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<CameraCapabilities> + Send + Sync + 'static,
	{
		self.capabilities = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::version`].
	pub fn with_version<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<VersionInfo> + Send + Sync + 'static,
	{
		self.version = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::ability_info`].
	pub fn with_ability_info<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<AbilityInfo> + Send + Sync + 'static,
	{
		self.ability_info = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::users`].
	pub fn with_users<F>(mut self, f: F) -> Self
	where
		F: Fn() -> CameraResult<UserList> + Send + Sync + 'static,
	{
		self.users = Some(Box::new(f));
		self
	}

	/// Install the closure invoked on [`DeviceAdmin::service`] for
	/// every [`ServiceKind`].
	pub fn with_service<F>(mut self, f: F) -> Self
	where
		F: Fn(ServiceKind) -> CameraResult<ServicePortState> + Send + Sync + 'static,
	{
		self.service = Some(Box::new(f));
		self
	}

	pub fn calls(&self) -> &FakeCalls {
		&self.calls
	}

	pub fn build(self) -> Arc<Self> {
		Arc::new(self)
	}
}

impl Default for FakeDeviceAdmin {
	fn default() -> Self {
		Self::new()
	}
}

#[async_trait]
impl DeviceAdmin for FakeDeviceAdmin {
	async fn capabilities(&self) -> CameraResult<CameraCapabilities> {
		match self.capabilities.as_ref() {
			Some(f) => f(),
			None => unset("capabilities"),
		}
	}

	async fn version(&self) -> CameraResult<VersionInfo> {
		match self.version.as_ref() {
			Some(f) => f(),
			None => unset("version"),
		}
	}

	async fn ability_info(&self) -> CameraResult<AbilityInfo> {
		match self.ability_info.as_ref() {
			Some(f) => f(),
			None => unset("ability_info"),
		}
	}

	async fn reboot(&self) -> CameraResult<()> {
		*self.calls.reboot.lock().unwrap() += 1;
		Ok(())
	}

	async fn set_time(&self, timestamp: OffsetDateTime) -> CameraResult<()> {
		self.calls.set_time.lock().unwrap().push(timestamp);
		Ok(())
	}

	async fn users(&self) -> CameraResult<UserList> {
		match self.users.as_ref() {
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
		self.calls
			.add_user
			.lock()
			.unwrap()
			.push((user_name, password, user_level));
		Ok(())
	}

	async fn modify_user(&self, user_name: String, password: String) -> CameraResult<()> {
		self.calls
			.modify_user
			.lock()
			.unwrap()
			.push((user_name, password));
		Ok(())
	}

	async fn delete_user(&self, user_name: String) -> CameraResult<()> {
		self.calls.delete_user.lock().unwrap().push(user_name);
		Ok(())
	}

	async fn service(&self, kind: ServiceKind) -> CameraResult<ServicePortState> {
		match self.service.as_ref() {
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
		self.calls
			.set_service
			.lock()
			.unwrap()
			.push((kind, enable, port));
		Ok(())
	}
}
