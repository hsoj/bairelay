use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};
use std::time::Instant;
use tokio::sync::mpsc::{channel, error::TryRecvError, Receiver};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_empty, reply_err_code, MockConnection,
};

/// Motion Status that the callback can send
#[derive(Clone, Copy, Debug)]
pub enum MotionStatus {
	/// Sent when motion is first detected
	Start(Instant),
	/// Sent when motion stops
	Stop(Instant),
	/// Sent when an Alarm about something other than motion was received
	NoChange(Instant),
}

/// A handle on current motion related events comming from the camera
///
/// When this object is dropped the motion events are stopped
pub struct MotionData {
	handle: JoinSet<Result<()>>,
	cancel: CancellationToken,
	rx: Receiver<Result<MotionStatus>>,
	last_update: MotionStatus,
	/// Cached at construction so the `Drop` impl can spawn its
	/// detached cancel-and-join task without calling
	/// `tokio::runtime::Handle::current()` — that call panics if Drop
	/// runs from a thread with no current runtime, which can happen
	/// if a `MotionData` ever crosses a non-tokio drop site.
	rt_handle: tokio::runtime::Handle,
}

impl MotionData {
	/// Consume the motion events diretly
	///
	/// An error is raised if the motion connection to the camera is dropped
	fn consume_motion_events(&mut self) -> Result<Vec<MotionStatus>> {
		let mut results: Vec<MotionStatus> = vec![];
		loop {
			match self.rx.try_recv() {
				Ok(motion) => results.push(motion?),
				Err(TryRecvError::Empty) => break,
				Err(e) => return Err(Error::from(e)),
			}
		}
		if let Some(last) = results.last() {
			self.last_update = *last;
		}
		Ok(results)
	}

	/// Await a new motion event
	///
	///
	pub async fn next_motion(&mut self) -> Result<MotionStatus> {
		let motions = self.consume_motion_events()?;
		if let Some(last) = motions.last() {
			Ok(*last)
		} else if let Some(motion) = self.rx.recv().await {
			let motion = motion?;
			self.last_update = motion;
			Ok(motion)
		} else {
			Err(Error::Other("Motion dropped"))
		}
	}
}

impl BcCamera {
	/// This message tells the camera to send the motion events to us
	/// Which are the received on msgid 33
	async fn start_motion_query(&self) -> Result<u16> {
		self.has_ability_rw("motion").await?;
		let connection = self.get_connection();

		let msg_num = self.new_message_num();
		let mut sub = connection.subscribe(MSG_ID_MOTION_REQUEST, msg_num).await?;
		let msg = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_MOTION_REQUEST,
				channel_id: self.channel_id,
				msg_num,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				..Default::default()
			}),
		};

		sub.send(msg).await?;

		let msg = sub.recv().await?;

		if let BcMeta {
			response_code: 200, ..
		} = msg.meta
		{
			Ok(msg_num)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "The camera did not accept the request to start motion",
			})
		}
	}

	/// This returns a data structure which can be used to
	/// query motion events
	pub async fn listen_on_motion(&self) -> Result<MotionData> {
		self.start_motion_query().await?;

		let connection = self.get_connection();

		// After start_motion_query (MSG_ID 31) the camera sends motion messages
		// when whenever motion is detected.
		let (tx, rx) = channel(20);

		let mut set = JoinSet::new();
		let channel_id = self.channel_id;
		let cancel = CancellationToken::new();
		let thread_cancel = cancel.clone();
		set.spawn(async move {
			tokio::select! {
				_ = thread_cancel.cancelled() => Result::Ok(()),
				v = async {
					let mut sub = connection.subscribe_to_id(MSG_ID_MOTION).await?;

					loop {
						tokio::task::yield_now().await;
						let msg = sub.recv().await;
						let status = match msg {
							Ok(motion_msg) => {
								if let BcBody::ModernMsg(ModernMsg {
									payload: Some(BcPayloads::BcXml(xml)),
									..
								}) = motion_msg.body
								{
									let alarm_event_list =
										xml.alarm_event_list.unwrap_or_default();
									let mut result = MotionStatus::NoChange(Instant::now());
									for alarm_event in &alarm_event_list.alarm_events {
										if alarm_event.channel_id == channel_id {
											if alarm_event.status != "none"
												|| alarm_event
													.ai_type
													.as_ref()
													.map(|ai_type| ai_type != "none")
													.unwrap_or(false)
											{
												result = MotionStatus::Start(Instant::now());
												break;
											} else {
												result = MotionStatus::Stop(Instant::now());
												break;
											}
										}
									}
									Ok(result)
								} else {
									Ok(MotionStatus::NoChange(Instant::now()))
								}
							}
							// On connection drop we stop
							Err(e) => Err(e),
						};

						if tx.send(status).await.is_err() {
							// Motion receiver has been dropped
							break;
						}
					}
					Ok(())
				} => v,
			}
		});

		Ok(MotionData {
			handle: set,
			cancel,
			rx,
			last_update: MotionStatus::NoChange(Instant::now()),
			rt_handle: tokio::runtime::Handle::current(),
		})
	}
}

impl MotionData {
	/// Construct a `MotionData` wired to a caller-supplied channel.
	///
	/// Gated on the `test-util` feature. Tests drive the returned handle
	/// by sending `MotionStatus` values on the matching `Sender` — no
	/// camera, no `JoinSet` task. The internal cancel token is unused
	/// (no background task to cancel) but is kept so `Drop` stays a
	/// no-op cancel.
	#[cfg(any(test, feature = "test-util"))]
	pub fn test_new(rx: tokio::sync::mpsc::Receiver<Result<MotionStatus>>) -> Self {
		Self {
			handle: JoinSet::new(),
			cancel: CancellationToken::new(),
			rx,
			last_update: MotionStatus::NoChange(Instant::now()),
			rt_handle: tokio::runtime::Handle::current(),
		}
	}
}

impl Drop for MotionData {
	fn drop(&mut self) {
		tracing::trace!("Drop MotionData");
		self.cancel.cancel();
		let mut handle = std::mem::take(&mut self.handle);
		// Use the cached runtime handle rather than `Handle::current()`
		// so Drop is safe on any thread.
		self.rt_handle.spawn(async move {
			while handle.join_next().await.is_some() {}
			tracing::trace!("Dropped MotionData");
		});
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	#[tokio::test]
	async fn listen_on_motion_happy_path_returns_handle() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_MOTION_REQUEST)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("motion", true).await;
		let md = cam.listen_on_motion().await.ok();
		assert!(md.is_some(), "listen_on_motion should succeed");
		// Drop MotionData; its Drop cancels the background task.
		drop(md);
	}

	#[tokio::test]
	async fn listen_on_motion_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_MOTION_REQUEST)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("motion", true).await;
		let err = cam.listen_on_motion().await.err().expect("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn listen_on_motion_missing_ability_returns_err() {
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.listen_on_motion().await.err().expect("should fail");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	// ---- MotionData state-machine tests driven by `test_new` ----

	#[tokio::test]
	async fn consume_motion_events_propagates_err() {
		let (tx, rx) = channel::<Result<MotionStatus>>(4);
		let mut md = MotionData::test_new(rx);
		tx.send(Err(Error::Other("boom"))).await.unwrap();
		tokio::task::yield_now().await;
		assert!(md.consume_motion_events().is_err());
	}

	#[tokio::test]
	async fn next_motion_consumes_buffered_event() {
		let (tx, rx) = channel::<Result<MotionStatus>>(4);
		let mut md = MotionData::test_new(rx);
		let t = Instant::now();
		tx.send(Ok(MotionStatus::Start(t))).await.unwrap();
		tokio::task::yield_now().await;
		let status = tokio::time::timeout(Duration::from_millis(200), md.next_motion())
			.await
			.expect("did not hang")
			.expect("ok");
		assert!(matches!(status, MotionStatus::Start(_)));
	}

	#[tokio::test]
	async fn next_motion_awaits_fresh_event() {
		let (tx, rx) = channel::<Result<MotionStatus>>(4);
		let mut md = MotionData::test_new(rx);
		// Nothing buffered; spawn producer.
		let h = tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(10)).await;
			tx.send(Ok(MotionStatus::Stop(Instant::now())))
				.await
				.unwrap();
		});
		let status = tokio::time::timeout(Duration::from_millis(500), md.next_motion())
			.await
			.expect("did not hang")
			.expect("ok");
		assert!(matches!(status, MotionStatus::Stop(_)));
		h.await.unwrap();
	}

	#[tokio::test]
	async fn next_motion_returns_err_when_channel_closed() {
		let (tx, rx) = channel::<Result<MotionStatus>>(4);
		let mut md = MotionData::test_new(rx);
		drop(tx);
		// With no sender alive, try_recv yields Disconnected inside
		// consume_motion_events → DroppedConnectionTry. If the branch
		// ever changes to hit `rx.recv().await` → None, the Other arm
		// also counts.
		let err = tokio::time::timeout(Duration::from_millis(200), md.next_motion())
			.await
			.expect("did not hang")
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::DroppedConnectionTry(_) | Error::Other(_)
		));
	}

	// ---- Inner listen_on_motion task body tests ----
	// These drive the MSG_ID_MOTION subscription loop by pushing frames
	// via the mock injector. The loop lives inside a spawned task inside
	// `listen_on_motion`; waiting on `md.next_motion()` drains it.

	fn push_motion_start(
		injector: &crate::baichuan::bc_protocol::connection::mock::MockInjector,
		channel_id: u8,
	) -> tokio::task::JoinHandle<()> {
		let injector = injector.clone();
		tokio::spawn(async move {
			let push = Bc::new_from_xml(
				BcMeta {
					msg_id: MSG_ID_MOTION,
					channel_id,
					msg_num: 0,
					stream_type: 0,
					response_code: 0,
					class: 0x6414,
				},
				BcXml {
					alarm_event_list: Some(AlarmEventList {
						version: "1".into(),
						alarm_events: vec![AlarmEvent {
							version: "1".into(),
							channel_id,
							status: "MD".into(),
							recording: 0,
							timeStamp: 0,
							ai_type: None,
						}],
					}),
					..Default::default()
				},
			);
			injector.push(push).await;
		})
	}

	#[tokio::test]
	async fn listen_on_motion_inner_task_emits_start_on_motion_frame() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_MOTION_REQUEST)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("motion", true).await;
		let mut md = cam.listen_on_motion().await.expect("ok");

		// Give the subscription time to register.
		tokio::time::sleep(Duration::from_millis(30)).await;
		let _ = push_motion_start(&injector, 0).await;

		let status = tokio::time::timeout(Duration::from_millis(500), md.next_motion())
			.await
			.expect("did not hang")
			.expect("ok");
		assert!(matches!(status, MotionStatus::Start(_)));
	}

	#[tokio::test]
	async fn listen_on_motion_inner_task_emits_stop_on_none_status_frame() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_MOTION_REQUEST)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("motion", true).await;
		let mut md = cam.listen_on_motion().await.expect("ok");

		tokio::time::sleep(Duration::from_millis(30)).await;
		let injector_c = injector.clone();
		tokio::spawn(async move {
			let push = Bc::new_from_xml(
				BcMeta {
					msg_id: MSG_ID_MOTION,
					channel_id: 0,
					msg_num: 0,
					stream_type: 0,
					response_code: 0,
					class: 0x6414,
				},
				BcXml {
					alarm_event_list: Some(AlarmEventList {
						version: "1".into(),
						alarm_events: vec![AlarmEvent {
							version: "1".into(),
							channel_id: 0,
							status: "none".into(),
							recording: 0,
							timeStamp: 0,
							ai_type: Some("none".into()),
						}],
					}),
					..Default::default()
				},
			);
			injector_c.push(push).await;
		});
		let status = tokio::time::timeout(Duration::from_millis(500), md.next_motion())
			.await
			.expect("did not hang")
			.expect("ok");
		assert!(matches!(status, MotionStatus::Stop(_)));
	}

	#[tokio::test]
	async fn listen_on_motion_inner_task_emits_start_on_ai_type_not_none() {
		// status=="none" but ai_type != "none" → fall into the "motion
		// detected" branch.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_MOTION_REQUEST)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("motion", true).await;
		let mut md = cam.listen_on_motion().await.expect("ok");

		tokio::time::sleep(Duration::from_millis(30)).await;
		let injector_c = injector.clone();
		tokio::spawn(async move {
			let push = Bc::new_from_xml(
				BcMeta {
					msg_id: MSG_ID_MOTION,
					channel_id: 0,
					msg_num: 0,
					stream_type: 0,
					response_code: 0,
					class: 0x6414,
				},
				BcXml {
					alarm_event_list: Some(AlarmEventList {
						version: "1".into(),
						alarm_events: vec![AlarmEvent {
							version: "1".into(),
							channel_id: 0,
							status: "none".into(),
							recording: 0,
							timeStamp: 0,
							ai_type: Some("person".into()),
						}],
					}),
					..Default::default()
				},
			);
			injector_c.push(push).await;
		});
		let status = tokio::time::timeout(Duration::from_millis(500), md.next_motion())
			.await
			.expect("did not hang")
			.expect("ok");
		assert!(matches!(status, MotionStatus::Start(_)));
	}

	#[tokio::test]
	async fn listen_on_motion_inner_task_returns_nochange_on_empty_xml() {
		// Frame without alarm_event_list → NoChange path.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_MOTION_REQUEST)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("motion", true).await;
		let mut md = cam.listen_on_motion().await.expect("ok");

		tokio::time::sleep(Duration::from_millis(30)).await;
		let injector_c = injector.clone();
		tokio::spawn(async move {
			let push = Bc::new_from_xml(
				BcMeta {
					msg_id: MSG_ID_MOTION,
					channel_id: 0,
					msg_num: 0,
					stream_type: 0,
					response_code: 0,
					class: 0x6414,
				},
				BcXml::default(),
			);
			injector_c.push(push).await;
		});
		let status = tokio::time::timeout(Duration::from_millis(500), md.next_motion())
			.await
			.expect("did not hang")
			.expect("ok");
		assert!(matches!(status, MotionStatus::NoChange(_)));
	}

	#[tokio::test]
	async fn listen_on_motion_inner_task_skips_other_channel_ids() {
		// Alarm event for a different channel_id → falls through to
		// NoChange.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_MOTION_REQUEST)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("motion", true).await;
		let mut md = cam.listen_on_motion().await.expect("ok");

		tokio::time::sleep(Duration::from_millis(30)).await;
		let injector_c = injector.clone();
		tokio::spawn(async move {
			let push = Bc::new_from_xml(
				BcMeta {
					msg_id: MSG_ID_MOTION,
					channel_id: 0,
					msg_num: 0,
					stream_type: 0,
					response_code: 0,
					class: 0x6414,
				},
				BcXml {
					alarm_event_list: Some(AlarmEventList {
						version: "1".into(),
						alarm_events: vec![AlarmEvent {
							version: "1".into(),
							channel_id: 7, // not our channel (0)
							status: "MD".into(),
							recording: 0,
							timeStamp: 0,
							ai_type: None,
						}],
					}),
					..Default::default()
				},
			);
			injector_c.push(push).await;
		});
		let status = tokio::time::timeout(Duration::from_millis(500), md.next_motion())
			.await
			.expect("did not hang")
			.expect("ok");
		assert!(matches!(status, MotionStatus::NoChange(_)));
	}

	/// `next_motion` falls through to `rx.recv().await` returning `None`
	/// (sender dropped while we were already past the buffered drain),
	/// surfacing `Other("Motion dropped")`.
	#[tokio::test]
	async fn next_motion_returns_other_when_sender_dropped_after_drain() {
		let (tx, rx) = channel::<Result<MotionStatus>>(4);
		let mut md = MotionData::test_new(rx);
		// Spawn a task that closes the sender after a short delay so
		// `next_motion` enters its inner `rx.recv().await`, which then
		// returns `None` and surfaces `Other("Motion dropped")`.
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(20)).await;
			drop(tx);
		});
		let err = tokio::time::timeout(Duration::from_millis(200), md.next_motion())
			.await
			.expect("did not hang")
			.expect_err("should fail");
		assert!(matches!(err, Error::Other("Motion dropped")));
	}
}
