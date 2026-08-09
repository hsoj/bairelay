use super::{BcCamera, Error, Result};
use crate::baichuan::bc::{model::*, xml::*};
use tokio::time::{interval, Duration};

#[cfg(test)]
use crate::baichuan::bc_protocol::connection::mock::{
	reply_200_xml, reply_err_code, MockConnection,
};

impl BcCamera {
	/// Helper to set the service state since they all share the same
	/// code. Single-source-of-truth gate: every public `set_*` (HTTP,
	/// HTTPS, RTSP, RTMP, ONVIF, ServerPort) flows through here, so
	/// gating once gates the whole module. Captured Argus XML
	/// (`tests/fixtures/<cam>.xml`) advertises `network/port_rw`,
	/// which is the unified key all six service-port flavours share.
	async fn set_services(&self, bcxml: Box<BcXml>) -> Result<()> {
		self.has_ability_rw("port").await?;
		let connection = self.get_connection();
		let msg_num = self.new_message_num();
		let mut sub_set = connection
			.subscribe(MSG_ID_SET_SERVICE_PORTS, msg_num)
			.await?;

		let get = Bc {
			meta: BcMeta {
				msg_id: MSG_ID_SET_SERVICE_PORTS,
				channel_id: self.channel_id,
				msg_num,
				response_code: 0,
				stream_type: 0,
				class: 0x6414,
			},
			body: BcBody::ModernMsg(ModernMsg {
				extension: None,
				payload: Some(BcPayloads::BcXml(bcxml)),
			}),
		};

		sub_set.send(get).await?;
		super::set_helpers::await_set_reply_with_quirk(
			&mut sub_set,
			super::set_helpers::SET_QUIRK_TIMEOUT,
		)
		.await
	}

	/// Helper since they all send the same message. Single-source gate
	/// for the public `get_*` accessors — `network/port_ro` (implied by
	/// `port_rw` per `populate_abilities`) is the unified ability key
	/// all six service-port flavours share. Note: `set_*` paths call
	/// `get_services` first to read-modify-write — they trigger this
	/// `_ro` gate before the stricter `_rw` gate in `set_services`.
	async fn get_services(&self) -> Result<Box<BcXml>> {
		self.has_ability_ro("port").await?;
		let connection = self.get_connection();
		let mut reties: usize = 0;
		let mut retry_interval = interval(Duration::from_millis(500));
		loop {
			retry_interval.tick().await;
			let msg_num = self.new_message_num();
			let mut sub_get = connection
				.subscribe(MSG_ID_GET_SERVICE_PORTS, msg_num)
				.await?;
			let get = Bc {
				meta: BcMeta {
					msg_id: MSG_ID_GET_SERVICE_PORTS,
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
				if let BcBody::ModernMsg(ModernMsg {
					payload: Some(BcPayloads::BcXml(xml)),
					..
				}) = msg.body
				{
					return Ok(xml);
				} else {
					return Err(Error::UnintelligibleReply {
						reply: std::sync::Arc::new(msg),
						why: "Expected ModernMsg payload but it was not received",
					});
				}
			}
		}
	}

	/// Get the [`ServerPort`] XML
	pub async fn get_serverport(&self) -> Result<ServerPort> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			server_port: Some(xml),
			..
		} = *bcxml
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected ServerPort xml but it was not received",
			})
		}
	}

	/// Set the server port
	pub async fn set_serverport(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			server_port: Some(mut xml),
			..
		} = *bcxml
		{
			if let Some(enabled) = set_on {
				xml.enable = Some({
					if enabled {
						1
					} else {
						0
					}
				});
			}
			if let Some(port) = set_port {
				xml.port = port;
			}
			self.set_services(Box::new(BcXml {
				server_port: Some(xml),
				..Default::default()
			}))
			.await
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected ServerPort xml but it was not received",
			})
		}
	}

	/// Get the [`HttpPort`] XML
	pub async fn get_http(&self) -> Result<HttpPort> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			http_port: Some(xml),
			..
		} = *bcxml
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected HttpPort xml but it was not received",
			})
		}
	}

	/// Set the http port
	pub async fn set_http(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			http_port: Some(mut xml),
			..
		} = *bcxml
		{
			if let Some(enabled) = set_on {
				xml.enable = Some({
					if enabled {
						1
					} else {
						0
					}
				});
			}
			if let Some(port) = set_port {
				xml.port = port;
			}
			self.set_services(Box::new(BcXml {
				http_port: Some(xml),
				..Default::default()
			}))
			.await
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected HttpPort xml but it was not received",
			})
		}
	}

	/// Get the [`HttpPort`] XML
	pub async fn get_https(&self) -> Result<HttpsPort> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			https_port: Some(xml),
			..
		} = *bcxml
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected HttpsPort xml but it was not received",
			})
		}
	}

	/// Set the https port
	pub async fn set_https(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			https_port: Some(mut xml),
			..
		} = *bcxml
		{
			if let Some(enabled) = set_on {
				xml.enable = Some({
					if enabled {
						1
					} else {
						0
					}
				});
			}
			if let Some(port) = set_port {
				xml.port = port;
			}
			self.set_services(Box::new(BcXml {
				https_port: Some(xml),
				..Default::default()
			}))
			.await
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected HttpsPort xml but it was not received",
			})
		}
	}

	/// Get the [`RtspPort`] XML
	pub async fn get_rtsp(&self) -> Result<RtspPort> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			rtsp_port: Some(xml),
			..
		} = *bcxml
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected RtspPort xml but it was not received",
			})
		}
	}

	/// Set the http port
	pub async fn set_rtsp(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			rtsp_port: Some(mut xml),
			..
		} = *bcxml
		{
			if let Some(enabled) = set_on {
				xml.enable = Some({
					if enabled {
						1
					} else {
						0
					}
				});
			}
			if let Some(port) = set_port {
				xml.port = port;
			}
			self.set_services(Box::new(BcXml {
				rtsp_port: Some(xml),
				..Default::default()
			}))
			.await
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected RtspPort xml but it was not received",
			})
		}
	}

	/// Get the [`RtmpPort`] XML
	pub async fn get_rtmp(&self) -> Result<RtmpPort> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			rtmp_port: Some(xml),
			..
		} = *bcxml
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected RtmpPort xml but it was not received",
			})
		}
	}

	/// Set the rtmp port
	pub async fn set_rtmp(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			rtmp_port: Some(mut xml),
			..
		} = *bcxml
		{
			if let Some(enabled) = set_on {
				xml.enable = Some({
					if enabled {
						1
					} else {
						0
					}
				});
			}
			if let Some(port) = set_port {
				xml.port = port;
			}
			self.set_services(Box::new(BcXml {
				rtmp_port: Some(xml),
				..Default::default()
			}))
			.await
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected RtmpPort xml but it was not received",
			})
		}
	}

	/// Get the [`OnvifPort`] XML
	pub async fn get_onvif(&self) -> Result<OnvifPort> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			onvif_port: Some(xml),
			..
		} = *bcxml
		{
			Ok(xml)
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected OnvifPort xml but it was not received",
			})
		}
	}

	/// Set the onvif port
	pub async fn set_onvif(&self, set_on: Option<bool>, set_port: Option<u32>) -> Result<()> {
		let bcxml = self.get_services().await?;
		if let BcXml {
			onvif_port: Some(mut xml),
			..
		} = *bcxml
		{
			if let Some(enabled) = set_on {
				xml.enable = Some({
					if enabled {
						1
					} else {
						0
					}
				});
			}
			if let Some(port) = set_port {
				xml.port = port;
			}
			self.set_services(Box::new(BcXml {
				onvif_port: Some(xml),
				..Default::default()
			}))
			.await
		} else {
			Err(Error::UnintelligibleXml {
				reply: std::sync::Arc::new(*bcxml),
				why: "Expected OnvifPort xml but it was not received",
			})
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::baichuan::bc_protocol::connection::mock::reply_200_empty;

	#[tokio::test]
	async fn get_serverport_without_ability_returns_missing_ability() {
		// Empty-abilities camera must short-circuit on the gate before
		// any wire I/O — proves the gate added 2026-05-01 fires.
		let mock = MockConnection::new().build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		let err = cam
			.get_serverport()
			.await
			.expect_err("must require ability");
		assert!(matches!(err, Error::MissingAbility { .. }));
	}

	/// Helper: mock a `get_services` reply carrying `bcxml`. Installs
	/// the `port` ability so the gate added 2026-05-01 doesn't short-
	/// circuit before the wire mock fires.
	async fn mock_get_services(bcxml: BcXml) -> BcCamera {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SERVICE_PORTS)
			.reply_with(move |req| reply_200_xml(req, bcxml))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("port", true).await;
		cam
	}

	#[tokio::test]
	async fn get_serverport_happy_path_parses_reply() {
		let cam = mock_get_services(BcXml {
			server_port: Some(ServerPort {
				version: "1.1".to_string(),
				port: 9000,
				enable: Some(1),
			}),
			..Default::default()
		})
		.await;
		let sp = cam.get_serverport().await.expect("ok");
		assert_eq!(sp.port, 9000);
		assert_eq!(sp.enable, Some(1));
	}

	#[tokio::test]
	async fn get_http_non_200_returns_err() {
		// 500 (not 400) is non-retryable -> CameraServiceUnavailable.
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SERVICE_PORTS)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("port", true).await;
		let err = cam.get_http().await.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	#[tokio::test]
	async fn get_serverport_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		let err = cam.get_serverport().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleXml { .. }));
	}

	#[tokio::test]
	async fn get_http_happy_path_parses_reply() {
		let cam = mock_get_services(BcXml {
			http_port: Some(HttpPort {
				version: "1.1".to_string(),
				port: 80,
				enable: Some(1),
			}),
			..Default::default()
		})
		.await;
		let p = cam.get_http().await.expect("ok");
		assert_eq!(p.port, 80);
	}

	#[tokio::test]
	async fn get_https_happy_path() {
		let cam = mock_get_services(BcXml {
			https_port: Some(HttpsPort {
				version: "1.1".to_string(),
				port: 443,
				enable: Some(1),
			}),
			..Default::default()
		})
		.await;
		assert_eq!(cam.get_https().await.expect("ok").port, 443);
	}

	#[tokio::test]
	async fn get_rtsp_happy_path() {
		let cam = mock_get_services(BcXml {
			rtsp_port: Some(RtspPort {
				version: "1.1".to_string(),
				port: 554,
				enable: Some(1),
			}),
			..Default::default()
		})
		.await;
		assert_eq!(cam.get_rtsp().await.expect("ok").port, 554);
	}

	#[tokio::test]
	async fn get_rtmp_happy_path() {
		let cam = mock_get_services(BcXml {
			rtmp_port: Some(RtmpPort {
				version: "1.1".to_string(),
				port: 1935,
				enable: Some(1),
			}),
			..Default::default()
		})
		.await;
		assert_eq!(cam.get_rtmp().await.expect("ok").port, 1935);
	}

	#[tokio::test]
	async fn get_onvif_happy_path() {
		let cam = mock_get_services(BcXml {
			onvif_port: Some(OnvifPort {
				version: "1.1".to_string(),
				port: 8000,
				enable: Some(1),
			}),
			..Default::default()
		})
		.await;
		assert_eq!(cam.get_onvif().await.expect("ok").port, 8000);
	}

	// Missing-XML error branches for each get_*.
	#[tokio::test]
	async fn get_http_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.get_http().await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn get_https_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.get_https().await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn get_rtsp_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.get_rtsp().await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn get_rtmp_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.get_rtmp().await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn get_onvif_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.get_onvif().await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	// set_*: happy path with both enable toggle and port updates.
	//
	// These tests pin the wire-shape of the read-modify-write round
	// trip: the SET request must echo unchanged fields verbatim and
	// land the requested update on the right service port. A naive
	// `reply_with(reply_200_empty)` would let a regression that wrote
	// the new port into the wrong service block (e.g. flipping
	// http_port → https_port on the wire) still pass.

	/// Two-step mock that runs an inspector closure on the SET request
	/// payload before answering with `reply_200_empty`. Per-set test
	/// uses this to pin which service block carries the update and
	/// what its post-merge fields look like.
	async fn mock_get_then_set_inspect<F>(bcxml: BcXml, inspect: F) -> BcCamera
	where
		F: FnOnce(&BcXml) + Send + Sync + 'static,
	{
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SERVICE_PORTS)
			.reply_with(move |req| reply_200_xml(req, bcxml))
			.expect_msg(MSG_ID_SET_SERVICE_PORTS)
			.reply_with_xml(move |req, xml| {
				inspect(xml);
				reply_200_empty(req)
			})
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("port", true).await;
		cam
	}

	#[tokio::test]
	async fn set_serverport_updates_port_and_enable() {
		let cam = mock_get_then_set_inspect(
			BcXml {
				server_port: Some(ServerPort {
					version: "1.1".to_string(),
					port: 9000,
					enable: Some(0),
				}),
				..Default::default()
			},
			|xml| {
				let sp = xml.server_port.as_ref().expect("server_port on SET");
				assert_eq!(sp.port, 9001);
				assert_eq!(sp.enable, Some(1));
				// Other service blocks must NOT be set on the write —
				// set_services takes a fresh BcXml per service so a
				// regression that piggy-backed extra blocks would
				// either confuse the camera or accidentally overwrite
				// other service config.
				assert!(xml.http_port.is_none());
				assert!(xml.https_port.is_none());
			},
		)
		.await;
		cam.set_serverport(Some(true), Some(9001))
			.await
			.expect("ok");
	}

	#[tokio::test]
	async fn set_http_disable_only() {
		let cam = mock_get_then_set_inspect(
			BcXml {
				http_port: Some(HttpPort {
					version: "1.1".to_string(),
					port: 80,
					enable: Some(1),
				}),
				..Default::default()
			},
			|xml| {
				let p = xml.http_port.as_ref().expect("http_port on SET");
				// enable flips to 0; port preserved verbatim.
				assert_eq!(p.enable, Some(0));
				assert_eq!(p.port, 80);
			},
		)
		.await;
		cam.set_http(Some(false), None).await.expect("ok");
	}

	#[tokio::test]
	async fn set_https_port_only() {
		let cam = mock_get_then_set_inspect(
			BcXml {
				https_port: Some(HttpsPort {
					version: "1.1".to_string(),
					port: 443,
					enable: Some(1),
				}),
				..Default::default()
			},
			|xml| {
				let p = xml.https_port.as_ref().expect("https_port on SET");
				// port lands new value; enable preserved verbatim.
				assert_eq!(p.port, 8443);
				assert_eq!(p.enable, Some(1));
			},
		)
		.await;
		cam.set_https(None, Some(8443)).await.expect("ok");
	}

	#[tokio::test]
	async fn set_rtsp_both_fields() {
		let cam = mock_get_then_set_inspect(
			BcXml {
				rtsp_port: Some(RtspPort {
					version: "1.1".to_string(),
					port: 554,
					enable: Some(0),
				}),
				..Default::default()
			},
			|xml| {
				let p = xml.rtsp_port.as_ref().expect("rtsp_port on SET");
				assert_eq!(p.port, 10554);
				assert_eq!(p.enable, Some(1));
			},
		)
		.await;
		cam.set_rtsp(Some(true), Some(10554)).await.expect("ok");
	}

	#[tokio::test]
	async fn set_rtmp_enable_only() {
		let cam = mock_get_then_set_inspect(
			BcXml {
				rtmp_port: Some(RtmpPort {
					version: "1.1".to_string(),
					port: 1935,
					enable: Some(0),
				}),
				..Default::default()
			},
			|xml| {
				let p = xml.rtmp_port.as_ref().expect("rtmp_port on SET");
				assert_eq!(p.enable, Some(1));
				assert_eq!(p.port, 1935);
			},
		)
		.await;
		cam.set_rtmp(Some(true), None).await.expect("ok");
	}

	#[tokio::test]
	async fn set_onvif_port_only() {
		let cam = mock_get_then_set_inspect(
			BcXml {
				onvif_port: Some(OnvifPort {
					version: "1.1".to_string(),
					port: 8000,
					enable: Some(1),
				}),
				..Default::default()
			},
			|xml| {
				let p = xml.onvif_port.as_ref().expect("onvif_port on SET");
				assert_eq!(p.port, 8080);
				assert_eq!(p.enable, Some(1));
			},
		)
		.await;
		cam.set_onvif(None, Some(8080)).await.expect("ok");
	}

	// set_*: no xml received -> UnintelligibleXml before the set call.
	#[tokio::test]
	async fn set_serverport_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.set_serverport(Some(true), None)
				.await
				.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn set_http_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.set_http(Some(true), None).await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn set_https_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.set_https(Some(true), None).await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn set_rtsp_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.set_rtsp(Some(true), None).await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn set_rtmp_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.set_rtmp(Some(true), None).await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	#[tokio::test]
	async fn set_onvif_missing_xml_returns_err() {
		let cam = mock_get_services(BcXml::default()).await;
		assert!(matches!(
			cam.set_onvif(Some(true), None).await.expect_err("fail"),
			Error::UnintelligibleXml { .. }
		));
	}

	// set_services: non-200 reply surfaces CameraServiceUnavailable.
	#[tokio::test]
	async fn set_serverport_non_200_returns_err() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SERVICE_PORTS)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						server_port: Some(ServerPort {
							version: "1.1".to_string(),
							port: 9000,
							enable: Some(1),
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_SET_SERVICE_PORTS)
			.reply_with(|req| reply_err_code(req, 500))
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("port", true).await;
		let err = cam
			.set_serverport(Some(true), Some(9001))
			.await
			.expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 500, .. }
		));
	}

	// get_services: 400 retryable up to 5 times, then surfaces error.
	#[tokio::test]
	async fn get_services_400_retries_then_fails() {
		// Six 400 replies → exhausts the 5-retry budget and returns
		// CameraServiceUnavailable. Use paused virtual clock so the
		// 500 ms retry interval does not actually sleep.
		tokio::time::pause();
		let mut mock = MockConnection::new();
		for _ in 0..6 {
			mock = mock
				.expect_msg(MSG_ID_GET_SERVICE_PORTS)
				.reply_with(|req| reply_err_code(req, 400));
		}
		let mock = mock.build().await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("port", true).await;

		let task = tokio::spawn(async move { cam.get_http().await });
		// Advance past 5 retry intervals plus a margin.
		for _ in 0..6 {
			tokio::time::advance(Duration::from_millis(600)).await;
			tokio::task::yield_now().await;
		}
		let err = task.await.expect("join").expect_err("should fail");
		assert!(matches!(
			err,
			Error::CameraServiceUnavailable { code: 400, .. }
		));
	}

	// get_services: 200 OK with no payload at all -> UnintelligibleReply.
	#[tokio::test]
	async fn get_services_missing_payload_returns_unintelligible() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SERVICE_PORTS)
			.reply_with(reply_200_empty)
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("port", true).await;
		let err = cam.get_http().await.expect_err("should fail");
		assert!(matches!(err, Error::UnintelligibleReply { .. }));
	}

	// set_services: no reply at all -> 500 ms timeout returns Ok(()).
	#[tokio::test]
	async fn set_serverport_no_reply_returns_ok() {
		let mock = MockConnection::new()
			.expect_msg(MSG_ID_GET_SERVICE_PORTS)
			.reply_with(|req| {
				reply_200_xml(
					req,
					BcXml {
						server_port: Some(ServerPort {
							version: "1.1".to_string(),
							port: 9000,
							enable: Some(1),
						}),
						..Default::default()
					},
				)
			})
			.expect_msg(MSG_ID_SET_SERVICE_PORTS)
			.reply_none()
			.build()
			.await;
		let cam = BcCamera::from_mock_connection(mock).await;
		cam.test_set_ability("port", true).await;
		cam.set_serverport(Some(true), Some(9001))
			.await
			.expect("no-reply path returns Ok");
	}
}
