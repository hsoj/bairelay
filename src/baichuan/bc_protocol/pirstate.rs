use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};
use tokio::time::{interval, Duration};

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_empty, reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Get the [RfAlarmCfg] xml which contains the PIR status of the camera
	pub async fn get_pirstate(&self) -> Result<RfAlarmCfg> {
		self.has_ability_ro("rfAlarm").await?;
		let connection = self.get_connection();
		let mut reties: usize = 0;
		let mut retry_interval = interval(Duration::from_millis(500));
		loop {
			retry_interval.tick().await;
			let msg_num = self.new_message_num();
			let mut sub_get = connection.subscribe(MSG_ID_GET_PIR_ALARM, msg_num).await?;
			let get = Bc {
				meta: BcMeta {
					msg_id: MSG_ID_GET_PIR_ALARM,
					channel_id: self.channel_id,
					msg_num,
					response_code: 0,
					stream_type: 0,
					class: 0x6414,
				},
				body: BcBody::ModernMsg(ModernMsg {
					extension: Some(Extension {
						rf_id: Some(self.channel_id),
						..Default::default()
					}),
					payload: None,
				}),
			};

			sub_get.send(get).await?;
			let msg = sub_get.recv().await?;
			if msg.meta.response_code == 400 {
				// Retryable
				if reties < 5 {
					reties += 1;
					continue;
				} else {
					return Err(Error::CameraServiceUnavailable {
						id: msg.meta.msg_id,
						code: msg.meta.response_code,
					});
				}
			} else if msg.meta.response_code != 200 {
				return Err(Error::CameraServiceUnavailable {
					id: msg.meta.msg_id,
					code: msg.meta.response_code,
				});
			} else {
				// Valid message with response_code == 200
				let mut msg = msg;
				if let BcBody::ModernMsg(ModernMsg {
					payload: Some(BcPayloads::BcXml(xml)),
					..
				}) = &mut msg.body
				{
					if let Some(pirstate) = xml.rf_alarm_cfg.take() {
						return Ok(pirstate);
					}
				}
				return Err(Error::UnintelligibleReply {
					reply: std::sync::Arc::new(msg),
					why: "Expected PirSate xml but it was not received",
				});
			}
		}
	}

	/// Set the PIR sensor using the [RfAlarmCfg] xml
	pub async fn set_pirstate(&self, rf_alarm_cfg: RfAlarmCfg) -> Result<()> {
		self.has_ability_rw("rfAlarm").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_set = connection
			.subscribe(MSG_ID_START_PIR_ALARM, msg_num)
			.await?;

		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_START_PIR_ALARM,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: Some(Extension {
					rf_id: Some(self.channel_id),
					..Default::default()
				}),
				payload: Some(BcPayloads::BcXml(Box::new(BcXml {
					rf_alarm_cfg: Some(rf_alarm_cfg),
					..Default::default()
				}))),
			}),
		};

		sub_set.send(get).await?;
		super::set_helpers::await_set_reply_with_quirk(
			&mut sub_set,
			super::set_helpers::SET_QUIRK_TIMEOUT,
		)
		.await
	}

	/// This is a convience function to control the PIR status
	/// True is on and false is off
	pub async fn pir_set(&self, state: bool) -> Result<()> {
		let mut pir_state = self.get_pirstate().await?;
		// println!("{:?}", pir_state);
		pir_state.enable = match state {
			true => 1,
			false => 0,
		};
		self.set_pirstate(pir_state).await?;
		Ok(())
	}
}

/// Turn PIR ON or OFF
pub enum PirState {
	/// Turn the PIR on
	On,
	/// Turn the PIR off
	Off,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_pirstate_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_PIR_ALARM)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						rf_alarm_cfg: Some(RfAlarmCfg {
							version: "1.1".to_string(),
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
		cam.test_set_ability("rfAlarm", false).await;
		let pir = cam.get_pirstate().await.expect("ok");
		assert_eq!(pir.enable, 1);
	}

	#[tokio::test]
	async fn get_pirstate_missing_ability_returns_err() {
		// No ability installed -> MissingAbility fires before any
		// request is sent.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_pirstate().await.expect_err("should fail");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	#[tokio::test]
	async fn get_pirstate_non_200_non_400_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_PIR_ALARM)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", false).await;
		let err = cam.get_pirstate().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_pirstate_missing_xml_returns_err() {
		// 200 OK but the rf_alarm_cfg field is absent.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_PIR_ALARM)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", false).await;
		let err = cam.get_pirstate().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn set_pirstate_missing_ability_returns_err() {
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.set_pirstate(RfAlarmCfg {
				version: "1.1".to_string(),
				enable: 1,
				..Default::default()
			})
			.await
			.expect_err("should fail");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	#[tokio::test]
	async fn set_pirstate_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_START_PIR_ALARM)
			.reply_with_xml(|req, xml| {
				let cfg = xml
					.rf_alarm_cfg
					.as_ref()
					.expect("rf_alarm_cfg on SET request");
				assert_eq!(cfg.version, "1.1");
				assert_eq!(cfg.enable, 1);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", true).await;
		cam.set_pirstate(RfAlarmCfg {
			version: "1.1".to_string(),
			enable: 1,
			..Default::default()
		})
		.await
		.expect("ok");
	}

	#[tokio::test]
	async fn set_pirstate_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_START_PIR_ALARM)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", true).await;
		let err = cam
			.set_pirstate(RfAlarmCfg {
				version: "1.1".to_string(),
				enable: 1,
				..Default::default()
			})
			.await
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn set_pirstate_no_reply_returns_ok() {
		// No reply inside 500 ms -> Ok().
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_START_PIR_ALARM)
			.reply_none()
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", true).await;
		cam.set_pirstate(RfAlarmCfg {
			version: "1.1".to_string(),
			enable: 1,
			..Default::default()
		})
		.await
		.expect("no-reply path returns Ok");
	}

	#[tokio::test]
	async fn pir_set_toggle_on_happy_path() {
		// pir_set first issues a get, flips enable, then a set. Pin
		// the wire shape of the SET so a regression that swapped the
		// true→1/false→0 mapping would fail this test.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_PIR_ALARM)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						rf_alarm_cfg: Some(RfAlarmCfg {
							version: "1.1".to_string(),
							enable: 0,
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_START_PIR_ALARM)
			.reply_with_xml(|req, xml| {
				let cfg = xml
					.rf_alarm_cfg
					.as_ref()
					.expect("rf_alarm_cfg on SET round-trip");
				assert_eq!(cfg.enable, 1, "pir_set(true) must send enable=1");
				assert_eq!(cfg.version, "1.1");
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", true).await;
		cam.pir_set(true).await.expect("ok");
	}

	#[tokio::test]
	async fn get_pirstate_retries_on_400_then_succeeds() {
		// First reply is 400 (retryable), second is 200 happy path.
		// Paused virtual clock so the 500 ms retry interval doesn't
		// burn real wall-time — same pattern as
		// `services.rs::get_services_400_retries_then_fails`.
		tokio::time::pause();
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_PIR_ALARM)
			.reply_with(|req| reply_err_code(req, 400))
			.expect_msg(MSG_ID_GET_PIR_ALARM)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						rf_alarm_cfg: Some(RfAlarmCfg {
							version: "1.1".to_string(),
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
		cam.test_set_ability("rfAlarm", false).await;
		let task = tokio::spawn(async move { cam.get_pirstate().await });
		// Advance past two retry intervals plus a margin.
		for _ in 0..3 {
			tokio::time::advance(Duration::from_millis(600)).await;
			tokio::task::yield_now().await;
		}
		task.await.expect("join").expect("returns Ok");
	}

	#[tokio::test]
	async fn get_pirstate_400_exhausts_retries() {
		// 6 x 400 responses exhaust the retry budget (max is 5).
		// Paused virtual clock — see sibling test above.
		tokio::time::pause();
		let mut mock = MockConnection::new();
		for _ in 0..6 {
			mock = mock
				.expect_msg(MSG_ID_GET_PIR_ALARM)
				.reply_with(|req| reply_err_code(req, 400));
		}
		let mock = mock.build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", false).await;
		let task = tokio::spawn(async move { cam.get_pirstate().await });
		// Advance past 5 retry intervals plus a margin.
		for _ in 0..6 {
			tokio::time::advance(Duration::from_millis(600)).await;
			tokio::task::yield_now().await;
		}
		let err = task
			.await
			.expect("join")
			.expect_err("retries exhausted → err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 400, .. }
		));
	}

	#[tokio::test]
	async fn pir_set_toggle_off_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_PIR_ALARM)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						rf_alarm_cfg: Some(RfAlarmCfg {
							version: "1.1".to_string(),
							enable: 1,
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_START_PIR_ALARM)
			.reply_with_xml(|req, xml| {
				let cfg = xml
					.rf_alarm_cfg
					.as_ref()
					.expect("rf_alarm_cfg on SET round-trip");
				assert_eq!(cfg.enable, 0, "pir_set(false) must send enable=0");
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("rfAlarm", true).await;
		cam.pir_set(false).await.expect("ok");
	}
}
