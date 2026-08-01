use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_empty, reply_err_code, MockConnection,
};

/// Specifies the phone type for the push notification
pub enum PhoneType {
	/// Specify that this is an ios push notfication
	///
	/// In this case the token must be the APNS
	Ios,
	/// Specify that this is an andriod push notfication
	///
	/// In this case the token must firebase cloud messaging token
	Android,
}

impl BcCamera {
	/// Convenience method for andriod of `[send_pushinfo]`
	pub async fn send_pushinfo_android(&self, token: &str, client_id: &str) -> Result<()> {
		self.send_pushinfo(token, client_id, PhoneType::Android)
			.await
	}
	/// Convenience method for andriod of `[send_pushinfo]`
	pub async fn send_pushinfo_ios(&self, token: &str, client_id: &str) -> Result<()> {
		self.send_pushinfo(token, client_id, PhoneType::Ios).await
	}
	/// Send the push info to regsiter for push notfications
	pub async fn send_pushinfo(
		&self,
		token: &str,
		client_id: &str,
		phone_type: PhoneType,
	) -> Result<()> {
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub = connection.subscribe(MSG_ID_PUSH_INFO, msg_num).await?;

		let phone_type_str = match phone_type {
			PhoneType::Ios => "reo_iphone",
			PhoneType::Android => "reo_fcm",
		};

		let msg = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_PUSH_INFO,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: Some(BcPayloads::BcXml(BcXml {
					push_info: Some(PushInfo {
						token: token.to_owned(),
						phone_type: phone_type_str.to_owned(),
						client_id: client_id.to_owned(),
					}),
					..Default::default()
				})),
			}),
		};

		sub.send(msg).await?;
		let msg = sub.recv().await?;
		if msg.meta.response_code != 200 {
			return Err(Error::CameraServiceUnavailable {
				id: msg.meta.msg_id,
				code: msg.meta.response_code,
			});
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn send_pushinfo_android_happy_path() {
		// Pin the wire-shape: Android → phone_type="reo_fcm" — a swap
		// to "reo_iphone" would still pass the previous shallow check
		// but silently send Android tokens through the iOS route.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PUSH_INFO)
			.reply_with_xml(|req, xml| {
				let pi = xml
					.push_info
					.as_ref()
					.expect("push_info on send_pushinfo request");
				assert_eq!(pi.phone_type, "reo_fcm");
				assert_eq!(pi.token, "fcm-token");
				assert_eq!(pi.client_id, "client-abc");
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.send_pushinfo_android("fcm-token", "client-abc")
			.await
			.expect("send_pushinfo_android should succeed");
	}

	#[tokio::test]
	async fn send_pushinfo_ios_happy_path() {
		// Symmetric coverage: iOS → phone_type="reo_iphone".
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PUSH_INFO)
			.reply_with_xml(|req, xml| {
				let pi = xml.push_info.as_ref().expect("push_info on iOS request");
				assert_eq!(pi.phone_type, "reo_iphone");
				assert_eq!(pi.token, "apns-token");
				assert_eq!(pi.client_id, "client-xyz");
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.send_pushinfo_ios("apns-token", "client-xyz")
			.await
			.expect("send_pushinfo_ios should succeed");
	}

	#[tokio::test]
	async fn send_pushinfo_ios_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PUSH_INFO)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.send_pushinfo_ios("apns-token", "client-xyz")
			.await
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}
}
