//! `MockConnection` — a scripted request→reply harness for driving
//! `BcCamera` command methods without a real socket.
// Test scaffolding behind the `test-util` feature — never in a release
// build — so a panic here is a test failure, same as `#[cfg(test)]`
// code, which clippy's in-tests exemption cannot see through a feature.
#![allow(clippy::expect_used)]
//!
//! Construction pattern:
//!
//! ```ignore
//! let conn = MockConnection::new()
//!     .expect_msg(MSG_ID_BATTERY_INFO)
//!     .reply_with(|req| Bc { /* scripted reply */ })
//!     .build();
//! let cam = BcCamera::from_mock_connection(conn);
//! let info = cam.battery_info().await.unwrap();
//! ```
//!
//! Scripted exchanges match on `msg_id`. When `BcCamera` sends a `Bc`
//! frame, the mock pops the next scripted exchange, asserts the id
//! matches, then runs the caller's reply closure. The closure gets the
//! request frame so it can echo `msg_num` / `channel_id` into the reply.
//!
//! Compiled unconditionally as part of `baichuan`'s public API.
//! Test helpers ship without feature gates because the bairelay
//! binary's own `#[cfg(test)]` modules import this module across the
//! crate boundary — `#[cfg(test)]` only fires for the crate being
//! compiled with tests, so cross-crate test seams cannot be gated that
//! way without introducing a Cargo feature. The release-build cost is a
//! small amount of dead-stripped code; production code never references
//! `MockConnection`. If a future PR ever takes a non-test dependency on
//! this module, that's the moment to revisit the gating.
//!
//! ## Scope
//!
//! Designed for **command-shape testing**: happy-path XML round-trips
//! and error-path reply classification. Not for stream subscription
//! testing (long-running `subscribe_to_id` with push semantics) nor for
//! socket-level failure modes — those belong in a lower-layer harness.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::sink::Sink;
use futures::stream::Stream;
use tokio::sync::mpsc::{channel, Sender};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

use super::{BcConnSink, BcConnSource, BcConnection};
use crate::baichuan::bc::model::*;
use crate::baichuan::Error;
use crate::baichuan::Result;

/// Closure that builds zero, one, or many replies from the request.
/// The mock invokes it the moment the request arrives, emitting each
/// `Bc` in order. Returning an empty `Vec` emulates "camera does not
/// answer" (use [`reply_none`](PendingReply::reply_none) for clarity).
pub type ReplyFn = Box<dyn FnOnce(&Bc) -> Vec<Bc> + Send + Sync>;

struct Expectation {
	msg_id: u32,
	reply: ReplyFn,
}

/// Builder for a [`MockConnection`]. Script exchanges with
/// [`expect_msg`] → [`reply_with`]; finalise with [`build`].
pub struct MockConnectionBuilder {
	expectations: VecDeque<Expectation>,
}

impl Default for MockConnectionBuilder {
	fn default() -> Self {
		Self::new()
	}
}

impl MockConnectionBuilder {
	/// Start a new mock with no scripted exchanges.
	pub fn new() -> Self {
		Self {
			expectations: VecDeque::new(),
		}
	}

	/// Declare the next request to match on `msg_id`. Returns a
	/// [`PendingReply`] which must be closed with [`reply_with`] or
	/// [`reply_none`].
	pub fn expect_msg(self, msg_id: u32) -> PendingReply {
		PendingReply {
			builder: self,
			msg_id,
		}
	}

	/// Finalise the script and produce a running [`MockConnection`].
	/// Spawns the mux task on the current tokio runtime.
	pub async fn build(self) -> MockConnection {
		MockConnection::start(self.expectations).await
	}
}

/// Half-finished exchange: `expect_msg` returns this, call
/// [`reply_with`] or [`reply_none`] to install the reply behaviour.
pub struct PendingReply {
	builder: MockConnectionBuilder,
	msg_id: u32,
}

impl PendingReply {
	/// Install a closure that builds the reply from the request. The
	/// closure is called once; returning `None` suppresses the reply
	/// (useful for error-path tests that exercise the
	/// `tokio::time::timeout` branch).
	pub fn reply_with<F>(mut self, f: F) -> MockConnectionBuilder
	where
		F: FnOnce(&Bc) -> Bc + Send + Sync + 'static,
	{
		let reply: ReplyFn = Box::new(move |bc| vec![f(bc)]);
		self.builder.expectations.push_back(Expectation {
			msg_id: self.msg_id,
			reply,
		});
		self.builder
	}

	/// Script "camera sends no reply" — the mock consumes the request
	/// but produces nothing. `BcCamera` methods that wrap
	/// `sub.recv()` in a `timeout()` treat this as success after the
	/// timeout fires.
	pub fn reply_none(mut self) -> MockConnectionBuilder {
		let reply: ReplyFn = Box::new(|_| vec![]);
		self.builder.expectations.push_back(Expectation {
			msg_id: self.msg_id,
			reply,
		});
		self.builder
	}

	/// Install a closure that builds an optional reply from the request
	/// — returning `None` suppresses the reply. Useful when the reply
	/// decision depends on request fields.
	pub fn reply_with_opt<F>(mut self, f: F) -> MockConnectionBuilder
	where
		F: FnOnce(&Bc) -> Option<Bc> + Send + Sync + 'static,
	{
		let reply: ReplyFn = Box::new(move |bc| match f(bc) {
			Some(r) => vec![r],
			None => vec![],
		});
		self.builder.expectations.push_back(Expectation {
			msg_id: self.msg_id,
			reply,
		});
		self.builder
	}

	/// Install a closure that builds an ordered sequence of replies
	/// from the request. Useful for commands like `get_snapshot` whose
	/// camera answers the initial XML then pushes binary chunks with
	/// different `msg_num`s on the same `msg_id`.
	pub fn reply_with_many<F>(mut self, f: F) -> MockConnectionBuilder
	where
		F: FnOnce(&Bc) -> Vec<Bc> + Send + Sync + 'static,
	{
		let reply: ReplyFn = Box::new(f);
		self.builder.expectations.push_back(Expectation {
			msg_id: self.msg_id,
			reply,
		});
		self.builder
	}

	/// Inspect-and-reply: extracts the request's `BcXml` payload and
	/// hands a borrowed reference to the closure alongside the raw
	/// `Bc` (so the caller can still echo `msg_num` / `channel_id`
	/// into the reply via [`reply_200_empty`] / [`reply_200_xml`]).
	///
	/// Panics if the request has no `BcXml` payload — use the plain
	/// [`reply_with`] for header-only requests, this helper is for
	/// set/control tests that want to pin the wire-shape of the
	/// request payload before answering.
	pub fn reply_with_xml<F>(mut self, f: F) -> MockConnectionBuilder
	where
		F: FnOnce(&Bc, &crate::baichuan::bc::xml::BcXml) -> Bc + Send + Sync + 'static,
	{
		let reply: ReplyFn = Box::new(move |bc| {
			let xml = inspect_xml(bc);
			vec![f(bc, xml)]
		});
		self.builder.expectations.push_back(Expectation {
			msg_id: self.msg_id,
			reply,
		});
		self.builder
	}

	/// Inspect-and-maybe-reply: same shape as [`reply_with_xml`] but
	/// the closure returns `Option<Bc>` so a test can assert the
	/// request payload then suppress the reply (drives the no-reply
	/// timeout branch). Panics if the request has no `BcXml` payload.
	pub fn reply_with_xml_opt<F>(mut self, f: F) -> MockConnectionBuilder
	where
		F: FnOnce(&Bc, &crate::baichuan::bc::xml::BcXml) -> Option<Bc> + Send + Sync + 'static,
	{
		let reply: ReplyFn = Box::new(move |bc| {
			let xml = inspect_xml(bc);
			match f(bc, xml) {
				Some(r) => vec![r],
				None => vec![],
			}
		});
		self.builder.expectations.push_back(Expectation {
			msg_id: self.msg_id,
			reply,
		});
		self.builder
	}
}

/// Borrow the `BcXml` carried by a `Bc` request. Panics with a
/// caller-friendly message if the request is header-only or carries
/// a binary payload — those shapes are caller errors when the test
/// is trying to inspect a set/control request.
pub fn inspect_xml(bc: &Bc) -> &crate::baichuan::bc::xml::BcXml {
	match &bc.body {
		BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(xml)),
			..
		}) => xml,
		_ => panic!(
			"inspect_xml: request msg_id={} has no BcXml payload",
			bc.meta.msg_id
		),
	}
}

/// A running mock connection. Holds the `Arc<BcConnection>` that
/// `BcCamera::from_mock_connection` consumes, plus the mux task
/// handle so tests can join on completion if they wish. Also carries
/// a side channel for injecting unsolicited messages (see
/// [`MockInjector`]).
pub struct MockConnection {
	pub(crate) inner: Arc<BcConnection>,
	_mux: Arc<Mutex<Option<JoinHandle<()>>>>,
	injector: MockInjector,
}

/// Side channel that lets a test push unsolicited inbound `Bc`
/// messages — e.g. camera-initiated binary chunks on a `msg_id`
/// the client has subscribed to via `subscribe_to_id`. Use this for
/// scripted push semantics that don't map onto the request→reply
/// builder.
#[derive(Clone)]
pub struct MockInjector {
	tx: Sender<Result<Bc>>,
}

impl MockInjector {
	/// Push an unsolicited inbound message into the mock source. Tests
	/// should await this after the client has installed its subscriber
	/// (otherwise the message will be dropped as uninteresting).
	pub async fn push(&self, bc: Bc) {
		let _ = self.tx.send(Ok(bc)).await;
	}
}

impl MockConnection {
	/// Shortcut: new empty builder. Returns `MockConnectionBuilder`
	/// (not `Self`) because `MockConnection` is only produced by
	/// `.build().await` at the end of the builder chain.
	#[allow(clippy::new_ret_no_self)]
	pub fn new() -> MockConnectionBuilder {
		MockConnectionBuilder::new()
	}

	/// Wire up the sink / source pair, spawn the mux task that pops
	/// scripted exchanges and emits replies, then construct the
	/// underlying `BcConnection`.
	async fn start(mut expectations: VecDeque<Expectation>) -> Self {
		// Test-side channels. BcCamera writes requests into `tx_req`
		// via the sink; the mux task reads from `rx_req`, pops the
		// next expectation, runs the closure, and pushes the reply
		// into `tx_reply` which BcCamera reads via the source.
		let (tx_req, mut rx_req) = channel::<Bc>(64);
		let (tx_reply, rx_reply) = channel::<Result<Bc>>(64);
		let injector_tx = tx_reply.clone();

		let mux = tokio::spawn(async move {
			while let Some(req) = rx_req.recv().await {
				let expectation = match expectations.pop_front() {
					Some(e) => e,
					None => {
						// No more scripted exchanges. Shut the
						// reply channel so `sub.recv()` on the
						// camera side returns `DroppedSubscriber`.
						tracing::warn!(
							"MockConnection: received req msg_id={} but no expectations left",
							req.meta.msg_id
						);
						break;
					}
				};
				assert_eq!(
					req.meta.msg_id, expectation.msg_id,
					"MockConnection: expected msg_id={} but got msg_id={}",
					expectation.msg_id, req.meta.msg_id
				);
				let replies = (expectation.reply)(&req);
				let mut closed = false;
				for reply in replies {
					if tx_reply.send(Ok(reply)).await.is_err() {
						closed = true;
						break;
					}
				}
				if closed {
					break;
				}
				// An empty Vec emulates a camera that doesn't answer;
				// callers wrap `sub.recv()` in a timeout to move on.
			}
		});

		let injector = MockInjector { tx: injector_tx };

		let sink: BcConnSink = Box::new(MockSink { tx: tx_req });
		let source: BcConnSource = Box::new(MockSource {
			rx: ReceiverStream::new(rx_reply),
		});

		let conn = BcConnection::new(sink, source)
			.await
			.expect("BcConnection::new over mock channels should not fail");

		MockConnection {
			inner: Arc::new(conn),
			_mux: Arc::new(Mutex::new(Some(mux))),
			injector,
		}
	}

	/// Clone-safe handle to the mock's inbound side channel. Tests use
	/// it to inject unsolicited messages after installing a subscriber.
	#[allow(dead_code)]
	pub fn injector(&self) -> MockInjector {
		self.injector.clone()
	}

	/// Borrow the inner [`BcConnection`] `Arc`. Public within the
	/// `bc_protocol` module so `BcCamera::from_mock_connection` can
	/// install it.
	pub(crate) fn into_arc(self) -> Arc<BcConnection> {
		self.inner
	}
}

/// Sink half: forwards each `Bc` request from `BcCamera` into the
/// mock mux task over a bounded channel.
struct MockSink {
	tx: Sender<Bc>,
}

impl Sink<Bc> for MockSink {
	type Error = Error;

	fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn start_send(self: Pin<&mut Self>, item: Bc) -> Result<()> {
		// Channel is bounded(64). If a test scripts more than 64
		// in-flight requests without a receiver catching up, this
		// will drop on the floor — acceptable: tests don't do that.
		match self.tx.try_send(item) {
			Ok(()) => Ok(()),
			Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
				Err(Error::Other("MockSink channel full"))
			}
			Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
				Err(Error::Other("MockSink channel closed"))
			}
		}
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<()>> {
		Poll::Ready(Ok(()))
	}
}

/// Source half: yields scripted replies to `BcCamera`.
struct MockSource {
	rx: ReceiverStream<Result<Bc>>,
}

impl Stream for MockSource {
	type Item = Result<Bc>;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		Pin::new(&mut self.rx).poll_next(cx)
	}
}

/// Build a standard modern-msg reply carrying the given `BcXml`,
/// echoing the request's `msg_num` / `channel_id` and tagging
/// `response_code = 200`. Convenience for command tests that don't
/// yet consume it — allow-dead-code until Stage 5's per-command
/// tests grow to use it.
#[allow(dead_code)]
pub fn reply_200_xml(req: &Bc, xml: crate::baichuan::bc::xml::BcXml) -> Bc {
	Bc::new_from_xml(
		BcMeta {
			msg_id: req.meta.msg_id,
			channel_id: req.meta.channel_id,
			msg_num: req.meta.msg_num,
			stream_type: 0,
			response_code: 200,
			class: 0x6414,
		},
		xml,
	)
}

/// Header-only 200 reply echoing the request's routing fields. Use
/// for commands whose happy-path reply carries no payload (reboot,
/// pir_set, floodlight manual, etc.).
#[allow(dead_code)]
pub fn reply_200_empty(req: &Bc) -> Bc {
	Bc {
		meta: BcMeta {
			msg_id: req.meta.msg_id,
			channel_id: req.meta.channel_id,
			msg_num: req.meta.msg_num,
			stream_type: 0,
			response_code: 200,
			class: 0x6414,
		},
		body: BcBody::ModernMsg(ModernMsg {
			extension: None,
			payload: None,
		}),
	}
}

/// Reply with a non-200 response code (no payload). Use for the
/// `CameraServiceUnavailable` error-path tests.
#[allow(dead_code)]
pub fn reply_err_code(req: &Bc, code: u16) -> Bc {
	Bc {
		meta: BcMeta {
			msg_id: req.meta.msg_id,
			channel_id: req.meta.channel_id,
			msg_num: req.meta.msg_num,
			stream_type: 0,
			response_code: code,
			class: 0x6414,
		},
		body: BcBody::ModernMsg(ModernMsg {
			extension: None,
			payload: None,
		}),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::BcCamera;

	/// Default + reply_with_opt Some-variant.
	#[tokio::test]
	async fn builder_default_and_reply_with_opt_some_returns_reply() {
		let builder: MockConnectionBuilder = MockConnectionBuilder::default();
		let mock = builder
			.expect_msg(1)
			.reply_with_opt(|req| Some(reply_200_empty(req)))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		let msg_num = cam.new_message_num();
		let mut sub = conn.subscribe(1, msg_num).await.unwrap();
		let req = Bc {
			meta: BcMeta {
				msg_id: 1,
				channel_id: 0,
				msg_num,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg::default()),
		};
		sub.send(req).await.unwrap();
		let reply = sub.recv().await.unwrap();
		assert_eq!(reply.meta.response_code, 200);
	}

	/// reply_with_opt None-variant suppresses the reply.
	#[tokio::test]
	async fn reply_with_opt_none_returns_no_reply() {
		let mock = MockConnection::new()
			.expect_msg(1)
			.reply_with_opt(|_req| None)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		let msg_num = cam.new_message_num();
		let mut sub = conn.subscribe(1, msg_num).await.unwrap();
		let req = Bc {
			meta: BcMeta {
				msg_id: 1,
				channel_id: 0,
				msg_num,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg::default()),
		};
		sub.send(req).await.unwrap();
		// No reply arrives — wrap in a short timeout so the test doesn't hang.
		let res = tokio::time::timeout(std::time::Duration::from_millis(50), sub.recv()).await;
		assert!(res.is_err(), "expected no reply");
	}

	#[tokio::test]
	async fn unhandle_msg_removes_registered_handler() {
		// Register via handle_msg, then unhandle, then register again —
		// second register should succeed (Vacant path), proving the
		// Remove command landed.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		conn.handle_msg(42, |_bc| Box::pin(async { None }))
			.await
			.unwrap();
		// Re-registering without unhandle first would fail with
		// SimultaneousSubscriptionId — not directly here because
		// AddHandler errors flow through the poller result channel,
		// not the send().
		conn.unhandle_msg(42).await.unwrap();
		conn.handle_msg(42, |_bc| Box::pin(async { None }))
			.await
			.unwrap();
	}

	/// reply_with_many emits multiple scripted replies.
	#[tokio::test]
	async fn reply_with_many_emits_sequence() {
		let mock = MockConnection::new()
			.expect_msg(1)
			.reply_with_many(|req| vec![reply_200_empty(req), reply_err_code(req, 500)])
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let conn = cam.get_connection();
		let msg_num = cam.new_message_num();
		let mut sub = conn.subscribe(1, msg_num).await.unwrap();
		let req = Bc {
			meta: BcMeta {
				msg_id: 1,
				channel_id: 0,
				msg_num,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg::default()),
		};
		sub.send(req).await.unwrap();
		let first = sub.recv().await.unwrap();
		assert_eq!(first.meta.response_code, 200);
		let second = sub.recv().await.unwrap();
		assert_eq!(second.meta.response_code, 500);
	}
}
