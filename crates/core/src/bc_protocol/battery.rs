//! Handles battery related messages
//!
//! There are primarily two messages:
//! - BatteryInfoList which the camera sends as part of its login info
//! - BatteryInfo which the client can request on demand
//!

use super::{BcCamera, PrintFormat, Result};
use crate::{
	bc::{model::*, xml::BatteryInfo},
	Error,
};

#[cfg(test)]
use crate::bc::xml::BcXml;
#[cfg(test)]
use crate::bc_protocol::connection::mock::{reply_200_xml, MockConnection};

impl BcCamera {
	/// Create a handller to respond to battery messages
	/// These messages are sent by the camera on login and maybe
	/// also on low battery events
	pub async fn monitor_battery(&self, format: PrintFormat) -> Result<()> {
		let connection = self.get_connection();
		connection
			.handle_msg(MSG_ID_BATTERY_INFO_LIST, move |bc| {
				Box::pin(async move {
					if let Bc {
						body:
							BcBody::ModernMsg(ModernMsg {
								payload:
									Some(BcPayloads::BcXml(BcXml {
										battery_list: Some(battery_list),
										..
									})),
								..
							}),
						..
					} = bc
					{
						for battery in battery_list.battery_info.iter() {
							match format {
								PrintFormat::None => {}
								PrintFormat::Human => {
									println!(
										"==Battery==\n\
                                    Charge: {}%,\n\
                                    Temperature: {}°C,\n\
                                    LowPower: {},\n\
                                    Adapter: {},\n\
                                    ChargeStatus: {},\n\
                                    ",
										battery.battery_percent,
										battery.temperature,
										if battery.low_power == 1 {
											"true"
										} else {
											"false"
										},
										battery.adapter_status,
										battery.charge_status,
									);
								}
								PrintFormat::Xml => {
									let bat_ser = String::from_utf8({
										let mut ser_buf = bytes::BytesMut::new();
										let parsed =
											quick_xml::se::to_writer(&mut ser_buf, &battery)
												.map(|_| ser_buf);
										parsed.expect("Could not serialise data").to_vec()
									})
									.expect("Should be UTF8");
									println!("{}", bat_ser);
								}
							}
						}
					}
					Option::<Bc>::None
				})
			})
			.await?;
		Ok(())
	}

	/// Requests the current battery status of the camera
	pub async fn battery_info(&self) -> Result<BatteryInfo> {
		let connection = self.get_connection();

		let msg_num = self.new_message_num();
		let mut sub = connection.subscribe(MSG_ID_BATTERY_INFO, msg_num).await?;

		let msg = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_BATTERY_INFO,
				channel_id: self.channel_id,
				msg_num,
				stream_type: 0,
				response_code: 0,
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

		sub.send(msg).await?;
		let msg = sub.recv().await?;

		if let Bc {
			meta: BcMeta {
				response_code: 200, ..
			},
			body:
				BcBody::ModernMsg(ModernMsg {
					payload:
						Some(BcPayloads::BcXml(BcXml {
							battery_info: Some(battery_info),
							..
						})),
					..
				}),
		} = msg
		{
			Ok(battery_info)
		} else {
			Err(Error::UnintelligibleReply {
				reply: std::sync::Arc::new(msg),
				why: "The camera did not accept the battery info (maybe no battery) command.",
			})
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn battery_info_happy_path_parses_reply() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_BATTERY_INFO)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						battery_info: Some(BatteryInfo {
							battery_percent: 73,
							temperature: 21,
							charge_status: "chargeComplete".to_string(),
							..Default::default()
						}),
						..Default::default()
					},
				)
			})
			.build()
			.await;

		let cam = BcCamera::from_mock_connection(mock).await;
		let info = cam
			.battery_info()
			.await
			.expect("battery_info should succeed");
		assert_eq!(info.battery_percent, 73);
		assert_eq!(info.temperature, 21);
		assert_eq!(info.charge_status, "chargeComplete");
	}

	#[tokio::test]
	async fn battery_info_non_200_returns_err() {
		// Empty reply (no battery_info xml) triggers the
		// `UnintelligibleReply` branch even with response_code 200.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_BATTERY_INFO)
			.reply_with(|req| reply_200_xml(req, BcXml::default()))
			.build()
			.await;

		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam.battery_info().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	async fn exercise_monitor_battery(format: PrintFormat) {
		use crate::bc::xml::{BatteryInfo, BatteryList};
		let mock = MockConnection::new().build().await;
		let injector = mock.injector();
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.monitor_battery(format).await.expect("register");

		// Let the AddHandler command reach the poller.
		tokio::task::yield_now().await;
		tokio::time::sleep(std::time::Duration::from_millis(20)).await;

		// Push a battery-info-list frame; closure body runs once.
		let push = Bc::new_from_xml(
			BcMeta {
				msg_id: MSG_ID_BATTERY_INFO_LIST,
				channel_id: 0,
				msg_num: 0,
				stream_type: 0,
				response_code: 0,
				class: 0x6414,
			},
			BcXml {
				battery_list: Some(BatteryList {
					battery_info: vec![BatteryInfo {
						battery_percent: 55,
						temperature: 25,
						low_power: 1,
						adapter_status: "charging".into(),
						charge_status: "chargeNormal".into(),
						..Default::default()
					}],
					..Default::default()
				}),
				..Default::default()
			},
		);
		injector.push(push).await;
		tokio::time::sleep(std::time::Duration::from_millis(30)).await;
	}

	#[tokio::test]
	async fn monitor_battery_none_format_silent_path() {
		exercise_monitor_battery(PrintFormat::None).await;
	}

	#[tokio::test]
	async fn monitor_battery_human_format_path() {
		exercise_monitor_battery(PrintFormat::Human).await;
	}

	#[tokio::test]
	async fn monitor_battery_xml_format_path() {
		exercise_monitor_battery(PrintFormat::Xml).await;
	}
}
