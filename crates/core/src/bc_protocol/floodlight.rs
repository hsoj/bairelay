use tokio::sync::mpsc::{channel, Receiver};

use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

#[cfg(test)]
use crate::bc_protocol::connection::mock::{
	reply_200_empty, reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Listen on the flood light update messages and return their XMLs
	pub async fn listen_on_floodlight(&self) -> Result<Receiver<FloodlightStatusList>> {
		let (tx, rx) = channel(3);
		let connection = self.get_connection();
		connection
			.handle_msg(MSG_ID_FLOODLIGHT_STATUS_LIST, move |bc| {
				let tx = tx.clone();
				Box::pin(async move {
					if let Bc {
						meta:
							BcMeta {
								msg_id: MSG_ID_FLOODLIGHT_STATUS_LIST,
								..
							},
						body:
							BcBody::ModernMsg(ModernMsg {
								payload:
									Some(BcPayloads::BcXml(BcXml {
										floodlight_status_list: Some(list),
										..
									})),
								..
							}),
					} = bc
					{
						let send_this: FloodlightStatusList = list.clone();
						let _ = tx.send(send_this).await;
					}
					None
				})
			})
			.await?;

		Ok(rx)
	}

	/// Set the floodlight status using the [FloodlightManual] xml
	pub async fn set_floodlight_manual(&self, state: bool, duration: u16) -> Result<()> {
		let connection = self.get_connection();

		let msg_num = self.new_message_num();
		let mut sub_set = connection
			.subscribe(MSG_ID_FLOODLIGHT_MANUAL, msg_num)
			.await?;

		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_FLOODLIGHT_MANUAL,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: Some(Extension {
					channel_id: Some(self.channel_id),
					..Default::default()
				}),
				payload: Some(BcPayloads::BcXml(BcXml {
					floodlight_manual: Some(FloodlightManual {
						version: "1".to_string(),
						channel_id: self.channel_id,
						status: match state {
							true => 1,
							false => 0,
						},
						duration,
					}),
					..Default::default()
				})),
			}),
		};

		sub_set.send(get).await?;
		super::set_helpers::await_set_reply_with_quirk(
			&mut sub_set,
			super::set_helpers::SET_QUIRK_TIMEOUT,
		)
		.await
	}

	/// Get the Flood Light tasks XML
	pub async fn get_floodlight_tasks(&self) -> Result<FloodlightTask> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection
			.subscribe(MSG_ID_FLOODLIGHT_TASKS_READ, msg_num)
			.await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_FLOODLIGHT_TASKS_READ,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: Some(Extension {
					channel_id: Some(self.channel_id),
					..Default::default()
				}),
				payload: None,
			}),
		};

		sub_get.send(get).await?;
		let msg = sub_get.recv().await?;
		if msg.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: msg.meta.msg_id,
				code: msg.meta.response_code,
			});
		}

		if let BcBody::ModernMsg(ModernMsg {
			payload: Some(BcPayloads::BcXml(BcXml {
				floodlight_task: Some(xml),
				..
			})),
			..
		}) = msg.body
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected FloodlightTask xml but it was not received",
			})
		}
	}

	/// Set the Flood Light tasks XML
	pub async fn set_floodlight_tasks(&self, new_xml: FloodlightTask) -> Result<()> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection
			.subscribe(MSG_ID_FLOODLIGHT_TASKS_WRITE, msg_num)
			.await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_FLOODLIGHT_TASKS_WRITE,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: Some(Extension {
					channel_id: Some(self.channel_id),
					..Default::default()
				}),
				payload: Some(BcPayloads::BcXml(BcXml {
					floodlight_task: Some(new_xml),
					..Default::default()
				})),
			}),
		};

		sub_get.send(get).await?;
		let msg = sub_get.recv().await?;
		if msg.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: msg.meta.msg_id,
				code: msg.meta.response_code,
			});
		}

		Ok(())
	}

	/// Convience function: Activate the Flood Light night mode
	pub async fn floodlight_tasks_enable(&self, state: bool) -> Result<()> {
		// Single round trip: re-use the fetched state instead of
		// calling `is_floodlight_tasks_enabled` (which itself
		// calls `get_floodlight_tasks`) and then `get_floodlight_tasks`
		// again.
		let mut curr_state = self.get_floodlight_tasks().await?;
		let want: u32 = u32::from(state);
		if curr_state.enable != want {
			curr_state.enable = want;
			self.set_floodlight_tasks(curr_state).await?;
		}
		Ok(())
	}

	/// Convience function: Check if Flood Light tasks are enbabled
	pub async fn is_floodlight_tasks_enabled(&self) -> Result<bool> {
		let curr_state = self.get_floodlight_tasks().await?;
		Ok(curr_state.enable == 1)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_floodlight_tasks_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_READ)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						floodlight_task: Some(FloodlightTask {
							version: "1.1".to_string(),
							channel: 0,
							enable: 1,
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let task = cam.get_floodlight_tasks().await.expect("ok");
		assert_eq!(task.enable, 1);
	}

	#[tokio::test]
	async fn get_floodlight_tasks_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_READ)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_floodlight_tasks().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_floodlight_tasks_missing_payload_returns_unintelligible() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_READ)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_floodlight_tasks().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn set_floodlight_tasks_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_WRITE)
			.reply_with_xml(|req, xml| {
				let task = xml
					.floodlight_task
					.as_ref()
					.expect("floodlight_task on SET request");
				assert_eq!(task.version, "1");
				assert_eq!(task.channel, 0);
				assert_eq!(task.enable, 1);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.set_floodlight_tasks(FloodlightTask {
			version: "1".into(),
			channel: 0,
			enable: 1,
			..Default::default()
		})
		.await
		.expect("ok");
	}

	#[tokio::test]
	async fn set_floodlight_tasks_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_WRITE)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.set_floodlight_tasks(FloodlightTask::default())
			.await
			.expect_err("err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn set_floodlight_manual_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_MANUAL)
			.reply_with_xml(|req, xml| {
				let m = xml
					.floodlight_manual
					.as_ref()
					.expect("floodlight_manual on SET request");
				// true → status=1 per the bool→u8 mapping in
				// set_floodlight_manual; pin to catch a swap.
				assert_eq!(m.status, 1);
				assert_eq!(m.duration, 30);
				assert_eq!(m.channel_id, 0);
				assert_eq!(m.version, "1");
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.set_floodlight_manual(true, 30).await.expect("ok");
	}

	#[tokio::test]
	async fn set_floodlight_manual_off_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_MANUAL)
			.reply_with_xml(|req, xml| {
				let m = xml
					.floodlight_manual
					.as_ref()
					.expect("floodlight_manual on SET request");
				// false → status=0 — the bool→u8 mapping flips here.
				assert_eq!(m.status, 0);
				assert_eq!(m.duration, 30);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.set_floodlight_manual(false, 30).await.expect("ok");
	}

	#[tokio::test]
	async fn set_floodlight_manual_no_reply_returns_ok() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_MANUAL)
			.reply_none()
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.set_floodlight_manual(true, 30)
			.await
			.expect("no-reply path returns Ok");
	}

	#[tokio::test]
	async fn set_floodlight_manual_non_200_returns_service_unavailable() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_MANUAL)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.set_floodlight_manual(true, 30).await.expect_err("err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn is_floodlight_tasks_enabled_true_and_false() {
		// enable=1 branch
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_READ)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						floodlight_task: Some(FloodlightTask {
							enable: 1,
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		assert!(cam.is_floodlight_tasks_enabled().await.unwrap());

		// enable=0 branch
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_READ)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						floodlight_task: Some(FloodlightTask {
							enable: 0,
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		assert!(!cam.is_floodlight_tasks_enabled().await.unwrap());
	}

	#[tokio::test]
	async fn floodlight_tasks_enable_short_circuits_when_already_correct() {
		// Already enabled → get then no set.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_READ)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						floodlight_task: Some(FloodlightTask {
							enable: 1,
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.floodlight_tasks_enable(true).await.expect("ok");
	}

	#[tokio::test]
	async fn floodlight_tasks_enable_drives_get_then_set() {
		// Currently disabled, want enabled → one read + one write.
		// (Pre-fix the path issued two reads via
		// `is_floodlight_tasks_enabled` before the write; collapsed
		// into a single fetch.)
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_READ)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						floodlight_task: Some(FloodlightTask {
							enable: 0,
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_FLOODLIGHT_TASKS_WRITE)
			.reply_with_xml(|req, xml| {
				let task = xml
					.floodlight_task
					.as_ref()
					.expect("floodlight_task on the WRITE round-trip");
				// The toggle write must flip enable to 1; a regression
				// to writing `0` would still drive a write through the
				// short-circuit but leave the camera disabled.
				assert_eq!(task.enable, 1);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.floodlight_tasks_enable(true).await.expect("ok");
	}

	#[tokio::test]
	async fn listen_on_floodlight_registers_and_closure_forwards_payload() {
		let mock = MockConnection::new().build().await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		let mut rx = cam.listen_on_floodlight().await.expect("register");

		// Let the AddHandler command reach the poller.
		tokio::task::yield_now().await;
		tokio::time::sleep(std::time::Duration::from_millis(20)).await;

		// Inject a status-list push frame; the closure should forward
		// the parsed list into the mpsc receiver.
		let push = Bc::new_from_xml(
			BcMeta {
				msg_id: MSG_ID_FLOODLIGHT_STATUS_LIST,
				channel_id: 0,
				msg_num: 0,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			BcXml {
				floodlight_status_list: Some(FloodlightStatusList {
					version: "1".into(),
					floodlight_status_list: vec![],
				}),
				..Default::default()
			},
		);
		injector.push(push).await;

		// Wait for the handler to forward via mpsc.
		let got = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
			.await
			.expect("receives within timeout")
			.expect("channel open");
		assert_eq!(got.version, "1");
	}
}
