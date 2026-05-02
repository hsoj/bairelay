use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

#[cfg(test)]
use crate::bc_protocol::connection::mock::{reply_200_xml, reply_err_code, MockConnection};

impl BcCamera {
	/// Get the [Support] xml which contains the ptz/talk support
	///
	pub async fn get_support(&self) -> Result<Support> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_GET_SUPPORT, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_GET_SUPPORT,
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
				support: Some(xml), ..
			})),
			..
		}) = msg.body
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected Support xml but it was not received",
			})
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_support_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SUPPORT)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						support: Some(Support {
							version: "1.1".to_string(),
							channel_num: Some(1),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let s = cam.get_support().await.expect("ok");
		assert_eq!(s.version, "1.1");
		assert_eq!(s.channel_num, Some(1));
	}

	#[tokio::test]
	async fn get_support_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SUPPORT)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_support().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_support_missing_payload_returns_unintelligible() {
		// 200 reply but no Support xml → UnintelligibleReply path.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SUPPORT)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_support().await.expect_err("no support → err");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}
}
