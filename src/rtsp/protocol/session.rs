//! RTSP session state machine.
//!
//! Tracks the state of a single RTSP session (distinct from a single
//! TCP connection — a connection may host zero or more sessions).

use std::time::{Duration, Instant};

/// Session state per RFC 7826.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
	/// Freshly created session; no track has been set up yet.
	Init,
	/// At least one track has been `SETUP`; ready to `PLAY`.
	Ready,
	/// Currently delivering media (after `PLAY`).
	Playing,
}

/// Result of handing a method to a session for state-transition validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
	/// Method is valid in the current state; state has been updated if needed.
	Allowed,
	/// Method is not valid in the current state; state is unchanged.
	WrongState,
	/// Session has been terminated (e.g. after `TEARDOWN`) and should be discarded.
	Terminated,
}

/// Generate a fresh cryptographically-random session ID (32 hex chars).
///
/// 128 bits of entropy — chosen so a long-lived server with many
/// concurrent clients has a vanishing birthday-collision probability over
/// its lifetime. RFC 7826 §18.49 merely requires "random … at least eight
/// octets"; we exceed that comfortably.
pub fn new_session_id() -> String {
	use rand::Rng;
	let bytes: [u8; 16] = rand::thread_rng().gen();
	bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A single RTSP session: an identifier, a state, and keepalive tracking.
pub struct Session {
	id: String,
	state: SessionState,
	last_activity: Instant,
	timeout: Duration,
}

impl Session {
	/// Create a new session with a fresh random ID, `Init` state, and the
	/// given keepalive `timeout`.
	pub fn new(timeout: Duration) -> Self {
		Self {
			id: new_session_id(),
			state: SessionState::Init,
			last_activity: Instant::now(),
			timeout,
		}
	}

	/// The session identifier (16 hex chars).
	pub fn id(&self) -> &str {
		&self.id
	}

	/// Current session state.
	pub fn state(&self) -> SessionState {
		self.state
	}

	/// Call on every RTSP request (or RTCP packet) belonging to this session
	/// to reset the keepalive timer.
	pub fn touch(&mut self) {
		self.last_activity = Instant::now();
	}

	/// Returns `true` if the session has not been touched within `timeout`.
	pub fn is_expired(&self) -> bool {
		self.last_activity.elapsed() >= self.timeout
	}

	/// The instant at which the session will be considered expired if not
	/// touched again.
	pub fn expires_at(&self) -> Instant {
		self.last_activity + self.timeout
	}

	/// Attempt a state transition triggered by an RTSP method.
	///
	/// Returns `Allowed` for valid transitions (state updated),
	/// `WrongState` if the method isn't valid in the current state (state unchanged),
	/// `Terminated` if the session is now ended.
	pub fn handle_method(&mut self, method: super::message::RtspMethod) -> Transition {
		use super::message::RtspMethod::*;
		match (self.state, method) {
			(_, Options) | (_, GetParameter) => Transition::Allowed,
			(SessionState::Ready | SessionState::Playing, Pause) => Transition::Allowed,
			(SessionState::Init, Pause) => Transition::WrongState,
			(_, Describe) => Transition::Allowed,
			(SessionState::Init, Setup) => {
				self.state = SessionState::Ready;
				Transition::Allowed
			}
			(SessionState::Ready, Setup) => Transition::Allowed, // additional track
			(SessionState::Ready, Play) | (SessionState::Playing, Play) => {
				self.state = SessionState::Playing;
				Transition::Allowed
			}
			(_, Teardown) => Transition::Terminated,
			(SessionState::Playing, Setup) => Transition::WrongState, // no re-SETUP while playing
			(SessionState::Init, Play) => Transition::WrongState,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::rtsp::protocol::message::RtspMethod;

	#[test]
	fn session_id_is_32_hex_chars() {
		let id = new_session_id();
		assert_eq!(id.len(), 32);
		assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
	}

	#[test]
	fn session_ids_are_distinct() {
		let ids: std::collections::HashSet<_> = (0..100).map(|_| new_session_id()).collect();
		assert_eq!(ids.len(), 100);
	}

	#[test]
	fn initial_state_is_init() {
		let s = Session::new(Duration::from_secs(30));
		assert_eq!(s.state(), SessionState::Init);
	}

	#[test]
	fn setup_transitions_init_to_ready() {
		let mut s = Session::new(Duration::from_secs(30));
		assert_eq!(s.handle_method(RtspMethod::Setup), Transition::Allowed);
		assert_eq!(s.state(), SessionState::Ready);
	}

	#[test]
	fn play_from_init_is_rejected() {
		let mut s = Session::new(Duration::from_secs(30));
		assert_eq!(s.handle_method(RtspMethod::Play), Transition::WrongState);
		assert_eq!(s.state(), SessionState::Init);
	}

	#[test]
	fn play_from_ready_enters_playing() {
		let mut s = Session::new(Duration::from_secs(30));
		s.handle_method(RtspMethod::Setup);
		s.handle_method(RtspMethod::Play);
		assert_eq!(s.state(), SessionState::Playing);
	}

	#[test]
	fn teardown_terminates_from_any_state() {
		for start in &[
			SessionState::Init,
			SessionState::Ready,
			SessionState::Playing,
		] {
			let mut s = Session::new(Duration::from_secs(30));
			s.state = *start;
			assert_eq!(
				s.handle_method(RtspMethod::Teardown),
				Transition::Terminated
			);
		}
	}

	#[test]
	fn options_always_allowed_never_changes_state() {
		let mut s = Session::new(Duration::from_secs(30));
		assert_eq!(s.handle_method(RtspMethod::Options), Transition::Allowed);
		assert_eq!(s.state(), SessionState::Init);
		s.handle_method(RtspMethod::Setup);
		assert_eq!(s.handle_method(RtspMethod::Options), Transition::Allowed);
		assert_eq!(s.state(), SessionState::Ready);
	}

	#[test]
	fn touch_resets_expiry() {
		let mut s = Session::new(Duration::from_millis(10));
		std::thread::sleep(Duration::from_millis(15));
		assert!(s.is_expired());
		s.touch();
		assert!(!s.is_expired());
	}

	#[test]
	fn pause_from_init_is_rejected() {
		let mut s = Session::new(Duration::from_secs(30));
		assert_eq!(s.handle_method(RtspMethod::Pause), Transition::WrongState);
		assert_eq!(s.state(), SessionState::Init);
	}

	#[test]
	fn pause_from_ready_is_allowed() {
		let mut s = Session::new(Duration::from_secs(30));
		s.handle_method(RtspMethod::Setup);
		assert_eq!(s.handle_method(RtspMethod::Pause), Transition::Allowed);
	}

	#[test]
	fn id_accessor_matches_constructor_output() {
		let s = Session::new(Duration::from_secs(30));
		let id = s.id();
		assert_eq!(id.len(), 32);
		assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
	}

	#[test]
	fn expires_at_advances_on_touch() {
		let mut s = Session::new(Duration::from_secs(60));
		let before = s.expires_at();
		std::thread::sleep(Duration::from_millis(5));
		s.touch();
		assert!(s.expires_at() > before);
	}

	#[test]
	fn describe_allowed_in_every_state() {
		let mut s = Session::new(Duration::from_secs(30));
		assert_eq!(s.handle_method(RtspMethod::Describe), Transition::Allowed);
		s.handle_method(RtspMethod::Setup);
		assert_eq!(s.handle_method(RtspMethod::Describe), Transition::Allowed);
		s.handle_method(RtspMethod::Play);
		assert_eq!(s.handle_method(RtspMethod::Describe), Transition::Allowed);
	}

	#[test]
	fn setup_in_ready_allows_additional_track() {
		let mut s = Session::new(Duration::from_secs(30));
		s.handle_method(RtspMethod::Setup); // -> Ready
		assert_eq!(s.handle_method(RtspMethod::Setup), Transition::Allowed);
		// Still Ready; a second track has been set up.
		assert_eq!(s.state(), SessionState::Ready);
	}

	#[test]
	fn setup_while_playing_is_rejected() {
		let mut s = Session::new(Duration::from_secs(30));
		s.handle_method(RtspMethod::Setup);
		s.handle_method(RtspMethod::Play);
		assert_eq!(s.handle_method(RtspMethod::Setup), Transition::WrongState);
		assert_eq!(s.state(), SessionState::Playing);
	}

	#[test]
	fn play_while_playing_is_idempotent() {
		let mut s = Session::new(Duration::from_secs(30));
		s.handle_method(RtspMethod::Setup);
		s.handle_method(RtspMethod::Play);
		assert_eq!(s.handle_method(RtspMethod::Play), Transition::Allowed);
		assert_eq!(s.state(), SessionState::Playing);
	}
}
