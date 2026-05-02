use super::{BcCamera, Result};
use crate::bc::{model::*, xml::*};
use std::sync::atomic::Ordering;

#[cfg(test)]
use crate::bc_protocol::connection::mock::{reply_200_empty, MockConnection};

impl BcCamera {
	/// Logout from the camera
	pub async fn logout(&self) -> Result<()> {
		if self.logged_in.load(Ordering::Relaxed) {
			let credentials = self.get_credentials();
			let connection = self.get_connection();
			let msg_num = self.new_message_num();
			let sub_logout = connection.subscribe(MSG_ID_LOGOUT, msg_num).await?;

			let username = credentials.username.clone();
			// Reolink's logout wire-shape echoes the plaintext password in
			// the XML. Surprising — most protocols only need a session
			// token at logout — but firmware reuses the login struct and
			// some models reject logout without it. Bytes travel over the
			// same AES-CFB-encrypted channel as login.
			let password = credentials.password.as_ref().cloned().unwrap_or_default();

			let modern_logout = Bc::new_from_xml(
				BcMeta {
					msg_id: MSG_ID_LOGOUT,
					channel_id: self.channel_id,
					msg_num,
					stream_type: 0,
					response_code: 0,
					class: 0x6414,
				},
				BcXml {
					login_user: Some(LoginUser {
						version: xml_ver(),
						user_name: username,
						password,
						user_ver: 1,
					}),
					login_net: Some(LoginNet::default()),
					..Default::default()
				},
			);

			sub_logout.send(modern_logout).await?;
		}
		self.logged_in.store(false, Ordering::Relaxed);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn logout_sends_logout_when_logged_in() {
		// `from_mock_connection` sets `logged_in = true`. `logout` is
		// fire-and-forget (no recv) so we just need to consume the
		// scripted message; the mock's presence of the expectation
		// asserts msg_id.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_LOGOUT)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.logout().await.expect("logout should succeed");
		assert!(!cam.logged_in.load(Ordering::Relaxed));
	}

	#[tokio::test]
	async fn logout_no_op_when_not_logged_in() {
		// logged_in = false means we skip the send entirely.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.logged_in.store(false, Ordering::Relaxed);
		cam.logout().await.expect("logout should succeed");
		assert!(!cam.logged_in.load(Ordering::Relaxed));
	}
}
