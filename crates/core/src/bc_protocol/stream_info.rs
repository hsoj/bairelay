use super::{BcCamera, Error, Result};
use crate::bc::{model::*, xml::*};

#[cfg(test)]
use crate::bc_protocol::connection::mock::{
	reply_200_empty, reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Get the [StreamInfoList] xml which contains the supported camera streams
	pub async fn get_stream_info(&self) -> Result<StreamInfoList> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection
			.subscribe(MSG_ID_STREAM_INFO_LIST, msg_num)
			.await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_STREAM_INFO_LIST,
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
				stream_info_list: Some(data),
				..
			})),
			..
		}) = msg.body
		{
			Ok(data)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "Expected StreamInfoList xml but it was not received",
			})
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn get_stream_info_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_STREAM_INFO_LIST)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						stream_info_list: Some(StreamInfoList {
							stream_infos: vec![StreamInfo {
								channel_bits: 1,
								encode_tables: vec![EncodeTable {
									name: "mainStream".to_string(),
									resolution: StreamResolution {
										width: 1920,
										height: 1080,
									},
									..Default::default()
								}],
							}],
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let info = cam.get_stream_info().await.expect("ok");
		assert_eq!(info.stream_infos.len(), 1);
		assert_eq!(info.stream_infos[0].encode_tables[0].name, "mainStream");
	}

	#[tokio::test]
	async fn get_stream_info_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_STREAM_INFO_LIST)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_stream_info().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_stream_info_missing_payload_returns_unintelligible() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_STREAM_INFO_LIST)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.get_stream_info().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}
}
