//! RTSP 1.0 message parsing and response construction.
//!
//! Thin layer over `rtsp-types`. We only parse methods we support;
//! everything else maps to 501 Not Implemented in the server layer.

use std::convert::TryFrom;

use rtsp_types::{
	HeaderName, HeaderValue, Message, Method, Request, Response, StatusCode, Version,
};
use thiserror::Error;

/// Errors that can occur while decoding an incoming RTSP request.
#[derive(Debug, Error)]
pub enum MessageError {
	/// The bytes could not be parsed as an RTSP request, or were a
	/// response/data message rather than a request.
	#[error("malformed RTSP message")]
	Malformed,
	/// The request used a method we do not accept (e.g. `RECORD`).
	#[error("unsupported method: {0}")]
	UnsupportedMethod(String),
	/// The mandatory `CSeq` header was absent or not a valid integer.
	#[error("missing CSeq header")]
	MissingCSeq,
}

/// RTSP methods the server accepts. Anything else maps to `501 Not Implemented`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspMethod {
	/// `OPTIONS` — advertise supported methods.
	Options,
	/// `DESCRIBE` — request an SDP description.
	Describe,
	/// `SETUP` — configure a single media stream transport.
	Setup,
	/// `PLAY` — start delivering media.
	Play,
	/// `TEARDOWN` — stop delivery and release the session.
	Teardown,
	/// `PAUSE` — temporarily halt delivery.
	Pause,
	/// `GET_PARAMETER` — keepalive / parameter query.
	GetParameter,
}

impl RtspMethod {
	/// Convert from a `rtsp-types` [`Method`]. Returns `None` for methods we
	/// do not implement.
	pub fn from_rtsp_types(m: &Method) -> Option<Self> {
		Some(match m {
			Method::Options => RtspMethod::Options,
			Method::Describe => RtspMethod::Describe,
			Method::Setup => RtspMethod::Setup,
			Method::Play => RtspMethod::Play,
			Method::Teardown => RtspMethod::Teardown,
			Method::Pause => RtspMethod::Pause,
			Method::GetParameter => RtspMethod::GetParameter,
			_ => return None,
		})
	}

	/// Uppercase wire representation, e.g. `OPTIONS`.
	pub fn as_str(self) -> &'static str {
		match self {
			RtspMethod::Options => "OPTIONS",
			RtspMethod::Describe => "DESCRIBE",
			RtspMethod::Setup => "SETUP",
			RtspMethod::Play => "PLAY",
			RtspMethod::Teardown => "TEARDOWN",
			RtspMethod::Pause => "PAUSE",
			RtspMethod::GetParameter => "GET_PARAMETER",
		}
	}
}

/// Parsed RTSP request, reduced to the fields the server acts on.
#[derive(Debug, Clone)]
pub struct ParsedRequest {
	/// The accepted method.
	pub method: RtspMethod,
	/// Full request-URI as a string.
	pub uri: String,
	/// `CSeq` value — required on every request.
	pub cseq: u32,
	/// `Session` header, with any `;timeout=N` suffix stripped.
	pub session: Option<String>,
	/// `Authorization` header, unparsed.
	pub authorization: Option<String>,
	/// `User-Agent` header, unparsed.
	pub user_agent: Option<String>,
	/// `Transport` header, unparsed. Further parsed by
	/// [`crate::rtsp::protocol::transport`].
	pub transport: Option<String>,
	/// `Range` header, unparsed.
	pub range: Option<String>,
}

/// Parse a buffer containing exactly one RTSP request.
///
/// Returns an error if the message is malformed, is not a request, uses an
/// unsupported method, or lacks a valid `CSeq` header.
pub fn parse_request(buf: &[u8]) -> Result<ParsedRequest, MessageError> {
	let (msg, _) = Message::<Vec<u8>>::parse(buf).map_err(|_| MessageError::Malformed)?;
	let req: Request<Vec<u8>> = match msg {
		Message::Request(r) => r,
		_ => return Err(MessageError::Malformed),
	};

	let method = RtspMethod::from_rtsp_types(req.method())
		.ok_or_else(|| MessageError::UnsupportedMethod(format!("{:?}", req.method())))?;
	let uri = req
		.request_uri()
		.ok_or(MessageError::Malformed)?
		.to_string();

	let cseq: u32 = req
		.headers()
		.find(|(n, _)| n.as_str().eq_ignore_ascii_case("CSeq"))
		.and_then(|(_, v)| v.as_str().trim().parse().ok())
		.ok_or(MessageError::MissingCSeq)?;

	let header_str = |name: &str| -> Option<String> {
		req.headers()
			.find(|(n, _)| n.as_str().eq_ignore_ascii_case(name))
			.map(|(_, v)| v.as_str().trim().to_string())
	};

	Ok(ParsedRequest {
		method,
		uri,
		cseq,
		session: header_str("Session").map(|s| {
			// Session header may carry a `;timeout=N` suffix; strip it.
			s.split(';').next().unwrap_or(&s).trim().to_string()
		}),
		authorization: header_str("Authorization"),
		user_agent: header_str("User-Agent"),
		transport: header_str("Transport"),
		range: header_str("Range"),
	})
}

/// Build a minimal RTSP response.
///
/// The `CSeq` and `Server` headers are always populated. Any caller-provided
/// `extra_headers` are appended verbatim. When `body` is `Some` the
/// `rtsp-types` builder automatically emits a `Content-Length` header
/// matching the body length.
pub fn build_response(
	status: StatusCode,
	cseq: u32,
	extra_headers: &[(&str, String)],
	body: Option<&[u8]>,
) -> Vec<u8> {
	let cseq_name = HeaderName::try_from("CSeq").expect("CSeq is ASCII");
	let server_name = HeaderName::try_from("Server").expect("Server is ASCII");

	let mut resp = Response::builder(Version::V1_0, status)
		.header(cseq_name, HeaderValue::from(cseq.to_string()))
		.header(server_name, HeaderValue::from("bairelay/0.1.0"));
	for (k, v) in extra_headers {
		let name = HeaderName::try_from(*k).expect("extra header name must be ASCII");
		resp = resp.header(name, HeaderValue::from(v.clone()));
	}
	let body_vec: Vec<u8> = body.map(|b| b.to_vec()).unwrap_or_default();
	let built = resp.build(body_vec);
	let mut out = Vec::new();
	built.write(&mut out).expect("RTSP response serialize");
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_options_request() {
		let raw = b"OPTIONS rtsp://host/cam1 RTSP/1.0\r\nCSeq: 1\r\n\r\n";
		let p = parse_request(raw).unwrap();
		assert_eq!(p.method, RtspMethod::Options);
		assert_eq!(p.cseq, 1);
		assert_eq!(p.uri, "rtsp://host/cam1");
	}

	#[test]
	fn parses_setup_with_transport() {
		let raw = b"SETUP rtsp://host/cam1/trackID=0 RTSP/1.0\r\nCSeq: 2\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n";
		let p = parse_request(raw).unwrap();
		assert_eq!(p.method, RtspMethod::Setup);
		assert!(p.transport.unwrap().contains("interleaved=0-1"));
	}

	#[test]
	fn parses_session_header_strips_timeout() {
		let raw =
			b"PLAY rtsp://host/cam1 RTSP/1.0\r\nCSeq: 3\r\nSession: abc123;timeout=60\r\n\r\n";
		let p = parse_request(raw).unwrap();
		assert_eq!(p.session, Some("abc123".to_string()));
	}

	#[test]
	fn missing_cseq_is_error() {
		let raw = b"OPTIONS rtsp://host/cam1 RTSP/1.0\r\n\r\n";
		assert!(matches!(parse_request(raw), Err(MessageError::MissingCSeq)));
	}

	#[test]
	fn response_has_cseq_and_server() {
		let bytes = build_response(StatusCode::Ok, 5, &[], None);
		let s = String::from_utf8(bytes).unwrap();
		assert!(s.contains("CSeq: 5"));
		assert!(s.contains("Server: bairelay/"));
	}

	#[test]
	fn response_with_body_includes_content_length() {
		let bytes = build_response(
			StatusCode::Ok,
			1,
			&[("Content-Type", "application/sdp".to_string())],
			Some(b"v=0\r\n"),
		);
		let s = String::from_utf8(bytes).unwrap();
		assert!(s.contains("Content-Length: 5"));
	}

	#[test]
	fn method_as_str_matches_wire_form() {
		assert_eq!(RtspMethod::Options.as_str(), "OPTIONS");
		assert_eq!(RtspMethod::Describe.as_str(), "DESCRIBE");
		assert_eq!(RtspMethod::Setup.as_str(), "SETUP");
		assert_eq!(RtspMethod::Play.as_str(), "PLAY");
		assert_eq!(RtspMethod::Teardown.as_str(), "TEARDOWN");
		assert_eq!(RtspMethod::Pause.as_str(), "PAUSE");
		assert_eq!(RtspMethod::GetParameter.as_str(), "GET_PARAMETER");
	}

	#[test]
	fn from_rtsp_types_covers_every_supported_method() {
		// Every supported method must round-trip through `from_rtsp_types`.
		assert_eq!(
			RtspMethod::from_rtsp_types(&Method::Options),
			Some(RtspMethod::Options)
		);
		assert_eq!(
			RtspMethod::from_rtsp_types(&Method::Describe),
			Some(RtspMethod::Describe)
		);
		assert_eq!(
			RtspMethod::from_rtsp_types(&Method::Setup),
			Some(RtspMethod::Setup)
		);
		assert_eq!(
			RtspMethod::from_rtsp_types(&Method::Play),
			Some(RtspMethod::Play)
		);
		assert_eq!(
			RtspMethod::from_rtsp_types(&Method::Teardown),
			Some(RtspMethod::Teardown)
		);
		assert_eq!(
			RtspMethod::from_rtsp_types(&Method::Pause),
			Some(RtspMethod::Pause)
		);
		assert_eq!(
			RtspMethod::from_rtsp_types(&Method::GetParameter),
			Some(RtspMethod::GetParameter)
		);
	}

	#[test]
	fn from_rtsp_types_rejects_unsupported_method() {
		// Methods we don't handle (e.g. RECORD, ANNOUNCE) must return None.
		assert_eq!(RtspMethod::from_rtsp_types(&Method::Record), None);
		assert_eq!(RtspMethod::from_rtsp_types(&Method::Announce), None);
	}

	#[test]
	fn parses_pause_request() {
		let raw = b"PAUSE rtsp://host/cam1 RTSP/1.0\r\nCSeq: 4\r\nSession: abc\r\n\r\n";
		let p = parse_request(raw).unwrap();
		assert_eq!(p.method, RtspMethod::Pause);
		assert_eq!(p.session.as_deref(), Some("abc"));
	}

	#[test]
	fn parses_get_parameter_request() {
		let raw = b"GET_PARAMETER rtsp://host/cam1 RTSP/1.0\r\nCSeq: 9\r\n\r\n";
		let p = parse_request(raw).unwrap();
		assert_eq!(p.method, RtspMethod::GetParameter);
		assert_eq!(p.cseq, 9);
	}

	#[test]
	fn non_request_message_is_malformed() {
		// A raw RTSP *response* is a valid parse but not a request — the
		// parser must reject it with Malformed.
		let raw = b"RTSP/1.0 200 OK\r\nCSeq: 1\r\n\r\n";
		assert!(matches!(parse_request(raw), Err(MessageError::Malformed)));
	}

	#[test]
	fn unsupported_method_yields_unsupported_error() {
		let raw = b"RECORD rtsp://host/cam1 RTSP/1.0\r\nCSeq: 1\r\n\r\n";
		assert!(matches!(
			parse_request(raw),
			Err(MessageError::UnsupportedMethod(_))
		));
	}

	#[test]
	fn build_response_appends_multiple_extra_headers() {
		let bytes = build_response(
			StatusCode::Ok,
			7,
			&[
				("Public", "OPTIONS, DESCRIBE".to_string()),
				("Cache-Control", "no-cache".to_string()),
			],
			None,
		);
		let s = String::from_utf8(bytes).unwrap();
		assert!(s.contains("Public: OPTIONS, DESCRIBE"));
		assert!(s.contains("Cache-Control: no-cache"));
	}
}
