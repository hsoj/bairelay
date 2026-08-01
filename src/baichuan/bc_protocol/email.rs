//! Email controlling methods.
//!
//! Configures the camera's **on-board SMTP client** — the firmware can
//! send its own emails on motion / alarm events without any cloud
//! account, given an SMTP server, credentials, and a per-day-of-week
//! schedule. None of this is on bairelay's data path; it would be
//! exposed only as one-shot CLI commands (`bairelay email <cam> ...`).
//!
//! **out of scope.** Out of scope for the current battery-camera
//! feature set (spec §10). The protocol surface is kept here as part
//! of the vendored `baichuan` so a future phase can wire it up
//! without a fresh round of reverse-engineering.
use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_empty, reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Get the current Email XML
	pub async fn get_email(&self) -> Result<Email> {
		// Captured Argus XML advertises `network/email_rw`; gate
		// matches the abilityValue suffix (rw → both ro and rw allowed).
		self.has_ability_ro("email").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_GET_EMAIL, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_GET_EMAIL,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
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
				email: Some(email), ..
			})),
			..
		}) = msg.body
		{
			Ok(email)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected Email xml but it was not received",
			})
		}
	}

	/// Set the Email XML
	pub async fn set_email(&self, email: Email) -> Result<()> {
		self.has_ability_rw("email").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_SET_EMAIL, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_SET_EMAIL,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: Some(BcPayloads::BcXml(BcXml {
					email: Some(email),
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

	/// Test the Email with this XML
	pub async fn test_email(&self, email: Email) -> Result<()> {
		self.has_ability_rw("email").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_TEST_EMAIL, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_TEST_EMAIL,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: Some(BcPayloads::BcXml(BcXml {
					email: Some(email),
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

	/// Get the current EmailTask XML
	pub async fn get_email_task(&self) -> Result<EmailTask> {
		self.has_ability_ro("email").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_GET_EMAIL_TASK, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_GET_EMAIL_TASK,
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
				email_task: Some(email_task),
				..
			})),
			..
		}) = msg.body
		{
			Ok(email_task)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected EmailTask xml but it was not received",
			})
		}
	}

	/// Setup the Email Task. Single gate for `set_email_task` /
	/// `email_on` / `email_off` / `email_on_always` (the latter three
	/// delegate here).
	pub async fn set_email_task(&self, email_task: EmailTask) -> Result<()> {
		self.has_ability_rw("email").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_SET_EMAIL_TASK, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_SET_EMAIL_TASK,
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
					email_task: Some(email_task),
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

	/// Turn on Email notifications
	pub async fn email_on(&self) -> Result<()> {
		self.set_email_task(EmailTask {
			version: xml_ver(),
			channel_id: self.channel_id,
			enable: 1,
			schedule_list: None,
		})
		.await
	}

	/// Turn off Email notifications
	pub async fn email_off(&self) -> Result<()> {
		self.set_email_task(EmailTask {
			version: xml_ver(),
			channel_id: self.channel_id,
			enable: 0,
			schedule_list: None,
		})
		.await
	}

	/// Turn on Email notifications all the time
	pub async fn email_on_always(&self) -> Result<()> {
		const DOW: [&str; 7] = [
			"Sunday",
			"Monday",
			"Tuesday",
			"Wednesday",
			"Thursday",
			"Friday",
			"Saturday",
		];
		self.set_email_task(EmailTask {
			version: xml_ver(),
			channel_id: self.channel_id,
			enable: 1,
			schedule_list: Some(ScheduleList {
				schedule: Schedule {
					alarm_type: "MD".to_owned(),
					time_block_list: TimeBlockList {
						time_block: DOW
							.iter()
							.map(|d| TimeBlock {
								enable: 1,
								week_day: d.to_string(),
								begin_hour: 0,
								end_hour: 23,
							})
							.collect(),
					},
				},
			}),
		})
		.await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_email_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_EMAIL)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						email: Some(Email {
							smtp_server: "smtp.example.com".to_string(),
							smtp_port: 465,
							user_name: "alerts".to_string(),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let email = cam.get_email().await.expect("ok");
		assert_eq!(email.smtp_server, "smtp.example.com");
		assert_eq!(email.smtp_port, 465);
	}

	#[tokio::test]
	async fn get_email_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_EMAIL)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let err = cam.get_email().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_email_missing_payload_returns_unintelligible() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_EMAIL)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let err = cam.get_email().await.expect_err("err");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn set_email_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_SET_EMAIL)
			.reply_with_xml(|req, xml| {
				let e = xml.email.as_ref().expect("email on SET request");
				assert_eq!(e.smtp_server, "smtp.example.com");
				assert_eq!(e.smtp_port, 465);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		cam.set_email(Email {
			smtp_server: "smtp.example.com".into(),
			smtp_port: 465,
			..Default::default()
		})
		.await
		.expect("ok");
	}

	#[tokio::test]
	async fn set_email_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_SET_EMAIL)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let err = cam.set_email(Email::default()).await.expect_err("err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn test_email_happy_path() {
		// `test_email` sends the candidate Email block on a different
		// msg_id than `set_email`. Pin that the email payload reaches
		// the wire — without this, a regression that dropped the
		// payload would still pass.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_TEST_EMAIL)
			.reply_with_xml(|req, xml| {
				let e = xml.email.as_ref().expect("email on test_email request");
				assert_eq!(e.smtp_server, "smtp.test.example");
				assert_eq!(e.smtp_port, 587);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		cam.test_email(Email {
			smtp_server: "smtp.test.example".into(),
			smtp_port: 587,
			..Default::default()
		})
		.await
		.expect("ok");
	}

	#[tokio::test]
	async fn test_email_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_TEST_EMAIL)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let err = cam.test_email(Email::default()).await.expect_err("err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_email_task_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_EMAIL_TASK)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						email_task: Some(EmailTask {
							version: "1".into(),
							channel_id: 0,
							enable: 1,
							schedule_list: None,
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let t = cam.get_email_task().await.expect("ok");
		assert_eq!(t.enable, 1);
	}

	#[tokio::test]
	async fn get_email_task_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_EMAIL_TASK)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let err = cam.get_email_task().await.expect_err("err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_email_task_missing_payload_returns_unintelligible() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_EMAIL_TASK)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let err = cam.get_email_task().await.expect_err("err");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	#[tokio::test]
	async fn set_email_task_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_SET_EMAIL_TASK)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		let err = cam
			.set_email_task(EmailTask::default())
			.await
			.expect_err("err");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn email_on_sets_task_enable_1() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_SET_EMAIL_TASK)
			.reply_with_xml(|req, xml| {
				let t = xml
					.email_task
					.as_ref()
					.expect("email_task on email_on request");
				// email_on must wire enable=1 and leave schedule_list
				// at None — `email_on` is a "honour the camera's
				// existing schedule" toggle, distinct from
				// `email_on_always`.
				assert_eq!(t.enable, 1);
				assert!(t.schedule_list.is_none());
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		cam.email_on().await.expect("ok");
	}

	#[tokio::test]
	async fn email_off_sets_task_enable_0() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_SET_EMAIL_TASK)
			.reply_with_xml(|req, xml| {
				let t = xml.email_task.as_ref().expect("email_task on email_off");
				assert_eq!(t.enable, 0);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		cam.email_off().await.expect("ok");
	}

	#[tokio::test]
	async fn get_email_without_ability_returns_missing_ability() {
		// Camera with empty abilities (`from_mock_connection` default)
		// must short-circuit on the gate before any wire I/O — proves
		// the gate added 2026-05-01 actually fires.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_email().await.expect_err("must require ability");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	#[tokio::test]
	async fn email_on_always_sets_7_day_schedule() {
		// The test name describes the semantics; pin the wire shape so
		// a regression that emitted a 6-day schedule (typo in DOW
		// const) or wrong hour bounds (e.g. begin=8/end=18) would fail
		// here instead of silently shipping a partial schedule.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_SET_EMAIL_TASK)
			.reply_with_xml(|req, xml| {
				let t = xml
					.email_task
					.as_ref()
					.expect("email_task on email_on_always");
				assert_eq!(t.enable, 1);
				let sched = t
					.schedule_list
					.as_ref()
					.expect("schedule_list populated on always-on");
				assert_eq!(sched.schedule.alarm_type, "MD");
				let blocks = &sched.schedule.time_block_list.time_block;
				assert_eq!(blocks.len(), 7, "must cover all 7 weekdays");
				let want_days = [
					"Sunday",
					"Monday",
					"Tuesday",
					"Wednesday",
					"Thursday",
					"Friday",
					"Saturday",
				];
				for (block, want_day) in blocks.iter().zip(want_days.iter()) {
					assert_eq!(block.week_day, *want_day);
					assert_eq!(block.enable, 1);
					assert_eq!(block.begin_hour, 0);
					assert_eq!(block.end_hour, 23);
				}
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("email", true).await;
		cam.email_on_always().await.expect("ok");
	}
}
