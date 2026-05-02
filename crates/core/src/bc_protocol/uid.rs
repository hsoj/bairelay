use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

#[cfg(test)]
use crate::bc_protocol::connection::mock::{
	reply_200_empty, reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Get the [Uid] xml which contains the uid of the camera
	pub async fn get_uid(&self) -> Result<Uid> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_UID, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_UID,
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
				uid: Some(uid_xml), ..
			})),
			..
		}) = msg.body
		{
			Ok(uid_xml)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected Uid xml but it was not received",
			})
		}
	}

	/// Get the UID
	pub async fn uid(&self) -> Result<String> {
		Ok(self.get_uid().await?.uid)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_uid_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_UID)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						uid: Some(Uid {
							version: "1.1".to_string(),
							uid: "95270005ABCDEFGH".to_string(),
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let uid = cam.uid().await.expect("ok");
		assert_eq!(uid, "95270005ABCDEFGH");
	}

	#[tokio::test]
	async fn get_uid_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_UID)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_uid().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_uid_missing_payload_returns_unintelligible() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_UID)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_uid().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}
}
