//! Trigger for the siren

use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_empty, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Trigger the siren
	pub async fn siren(&self) -> Result<()> {
		// Gate on the `siren` ability — battery-only Argus cameras and
		// many fixed-mount models lack a siren entirely. Without this
		// gate the camera replies with a non-200 that the dispatcher
		// classifies as a generic protocol error (exit 5); with it the
		// CLI surfaces the more accurate "feature unsupported" exit 6.
		self.has_ability_rw("siren").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_get = connection.subscribe(MSG_ID_PLAY_AUDIO, msg_num).await?;
		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_PLAY_AUDIO,
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
				payload: Some(BcPayloads::BcXml(Box::new(BcXml {
					audio_play_info: Some(AudioPlayInfo {
						channel_id: self.channel_id,
						play_mode: 0,
						play_duration: 0,
						play_times: 1,
						on_off: 0,
					}),
					..Default::default()
				}))),
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
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn siren_happy_path() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PLAY_AUDIO)
			.reply_with_xml(|req, xml| {
				let info = xml
					.audio_play_info
					.as_ref()
					.expect("audio_play_info on siren request");
				// play_times=1 is the load-bearing field — Reolink
				// fires the siren once per request; a regression to 0
				// silently turns the operator's `bairelay siren` into
				// a no-op.
				assert_eq!(info.play_times, 1);
				assert_eq!(info.play_mode, 0);
				assert_eq!(info.play_duration, 0);
				assert_eq!(info.on_off, 0);
				assert_eq!(info.channel_id, 0);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("siren", true).await;
		cam.siren().await.expect("siren should succeed");
	}

	#[tokio::test]
	async fn siren_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_PLAY_AUDIO)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("siren", true).await;
		let err = cam.siren().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn siren_missing_ability_returns_err() {
		// Fresh camera with no abilities populated → siren must
		// surface as MissingAbility (exit code 6, "unsupported"),
		// not a generic protocol error.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.siren().await.expect_err("should fail");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}
}
