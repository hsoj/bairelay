//! RTSP `Transport:` header parsing and response generation.

use thiserror::Error;

/// Parsed representation of a client-requested `Transport:` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportSpec {
	/// RTP-over-RTSP interleaved on the existing TCP connection.
	TcpInterleaved {
		/// Interleaved channel carrying RTP packets.
		channel_rtp: u8,
		/// Interleaved channel carrying RTCP packets.
		channel_rtcp: u8,
	},
	/// Plain UDP unicast with client-supplied ports.
	UdpUnicast {
		/// Client's RTP reception port.
		client_rtp_port: u16,
		/// Client's RTCP reception port.
		client_rtcp_port: u16,
	},
}

/// Errors produced while parsing a `Transport:` header.
#[derive(Debug, Error)]
pub enum TransportError {
	/// The requested transport profile or mode is not supported by this server.
	#[error("unsupported transport: {0}")]
	Unsupported(String),
	/// The header is syntactically invalid.
	#[error("malformed transport header: {0}")]
	Malformed(String),
}

/// Parse a `Transport:` header into our simplified model.
///
/// Accepts:
/// - `RTP/AVP/TCP;unicast;interleaved=N-M`
/// - `RTP/AVP;unicast;client_port=N-M` (UDP unicast)
/// - `RTP/AVP/UDP;unicast;client_port=N-M` (explicit UDP profile)
///
/// Rejects multicast. Extra unknown parameters (e.g. `ssrc=...`, `mode=...`)
/// are tolerated and ignored.
pub fn parse(value: &str) -> Result<TransportSpec, TransportError> {
	let trimmed = value.trim();
	let mut parts = trimmed.split(';').map(str::trim);
	let profile = parts
		.next()
		.ok_or_else(|| TransportError::Malformed("empty".into()))?;
	let is_tcp = profile.eq_ignore_ascii_case("RTP/AVP/TCP");
	let is_udp =
		profile.eq_ignore_ascii_case("RTP/AVP") || profile.eq_ignore_ascii_case("RTP/AVP/UDP");
	if !is_tcp && !is_udp {
		return Err(TransportError::Unsupported(profile.to_string()));
	}

	let mut cast = None;
	let mut interleaved: Option<(u8, u8)> = None;
	let mut client_port: Option<(u16, u16)> = None;

	for part in parts {
		if part.eq_ignore_ascii_case("unicast") {
			cast = Some("unicast");
		} else if part.eq_ignore_ascii_case("multicast") {
			return Err(TransportError::Unsupported("multicast".into()));
		} else if let Some(rest) = part.strip_prefix_ignore_case("interleaved=") {
			let (a, b) = parse_range_u8(rest)?;
			interleaved = Some((a, b));
		} else if let Some(rest) = part.strip_prefix_ignore_case("client_port=") {
			let (a, b) = parse_range_u16(rest)?;
			client_port = Some((a, b));
		}
		// ignore ssrc, mode, etc.
	}

	if is_tcp {
		let (rtp, rtcp) = interleaved
			.ok_or_else(|| TransportError::Malformed("TCP without interleaved".into()))?;
		return Ok(TransportSpec::TcpInterleaved {
			channel_rtp: rtp,
			channel_rtcp: rtcp,
		});
	}
	if is_udp {
		let (rtp, rtcp) = client_port
			.ok_or_else(|| TransportError::Malformed("UDP without client_port".into()))?;
		if cast != Some("unicast") {
			// assume unicast if not specified
		}
		return Ok(TransportSpec::UdpUnicast {
			client_rtp_port: rtp,
			client_rtcp_port: rtcp,
		});
	}
	unreachable!()
}

/// Build the server-side `Transport:` response header for a TCP session.
///
/// Appends `ssrc=<8-digit hex>` so the client can demultiplex RTP packets
/// that may arrive before the PLAY response (RFC 2326 §12.39). ffmpeg
/// and other strict clients rely on this.
pub fn build_tcp_response(channel_rtp: u8, channel_rtcp: u8, ssrc: u32) -> String {
	format!("RTP/AVP/TCP;unicast;interleaved={channel_rtp}-{channel_rtcp};ssrc={ssrc:08X}")
}

/// Build the server-side `Transport:` response header for a UDP session.
///
/// Appends `ssrc=<8-digit hex>` so the client can demultiplex RTP packets
/// that may arrive before the PLAY response (RFC 2326 §12.39).
pub fn build_udp_response(
	client_rtp: u16,
	client_rtcp: u16,
	server_rtp: u16,
	server_rtcp: u16,
	ssrc: u32,
) -> String {
	format!(
		"RTP/AVP;unicast;client_port={client_rtp}-{client_rtcp};\
		 server_port={server_rtp}-{server_rtcp};ssrc={ssrc:08X}"
	)
}

fn parse_range_u8(s: &str) -> Result<(u8, u8), TransportError> {
	let (a, b) = s
		.split_once('-')
		.ok_or_else(|| TransportError::Malformed(format!("not a range: {s}")))?;
	let a: u8 = a
		.trim()
		.parse()
		.map_err(|e| TransportError::Malformed(format!("bad u8: {e}")))?;
	let b: u8 = b
		.trim()
		.parse()
		.map_err(|e| TransportError::Malformed(format!("bad u8: {e}")))?;
	Ok((a, b))
}

fn parse_range_u16(s: &str) -> Result<(u16, u16), TransportError> {
	let (a, b) = s
		.split_once('-')
		.ok_or_else(|| TransportError::Malformed(format!("not a range: {s}")))?;
	let a: u16 = a
		.trim()
		.parse()
		.map_err(|e| TransportError::Malformed(format!("bad u16: {e}")))?;
	let b: u16 = b
		.trim()
		.parse()
		.map_err(|e| TransportError::Malformed(format!("bad u16: {e}")))?;
	Ok((a, b))
}

trait StrExt {
	fn strip_prefix_ignore_case<'a>(&'a self, prefix: &str) -> Option<&'a str>;
}

impl StrExt for str {
	fn strip_prefix_ignore_case<'a>(&'a self, prefix: &str) -> Option<&'a str> {
		if self.len() >= prefix.len() && self[..prefix.len()].eq_ignore_ascii_case(prefix) {
			Some(&self[prefix.len()..])
		} else {
			None
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_tcp_interleaved() {
		let t = parse("RTP/AVP/TCP;unicast;interleaved=0-1").unwrap();
		assert_eq!(
			t,
			TransportSpec::TcpInterleaved {
				channel_rtp: 0,
				channel_rtcp: 1
			}
		);
	}

	#[test]
	fn parses_udp_unicast() {
		let t = parse("RTP/AVP;unicast;client_port=5000-5001").unwrap();
		assert_eq!(
			t,
			TransportSpec::UdpUnicast {
				client_rtp_port: 5000,
				client_rtcp_port: 5001
			}
		);
	}

	#[test]
	fn rejects_multicast() {
		assert!(matches!(
			parse("RTP/AVP;multicast"),
			Err(TransportError::Unsupported(_))
		));
	}

	#[test]
	fn rejects_unknown_profile() {
		assert!(matches!(
			parse("SRTP/AVP"),
			Err(TransportError::Unsupported(_))
		));
	}

	#[test]
	fn tolerates_extra_fields() {
		let t = parse("RTP/AVP/TCP;unicast;interleaved=2-3;ssrc=ABCDEF").unwrap();
		assert_eq!(
			t,
			TransportSpec::TcpInterleaved {
				channel_rtp: 2,
				channel_rtcp: 3
			}
		);
	}

	#[test]
	fn builds_tcp_response_format() {
		assert_eq!(
			build_tcp_response(0, 1, 0xDEADBEEF),
			"RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=DEADBEEF"
		);
	}

	#[test]
	fn builds_udp_response_format() {
		assert_eq!(
			build_udp_response(5000, 5001, 40000, 40001, 0x01020304),
			"RTP/AVP;unicast;client_port=5000-5001;\
			 server_port=40000-40001;ssrc=01020304"
		);
	}
}
