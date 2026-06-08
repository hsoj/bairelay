//! Offline decoder for captured Reolink Baichuan-over-UDP sessions.
//!
//! Reads a tcpdump capture, demuxes UDP datagrams between the camera and
//! one peer (bairelay or the official client), drives
//! `bairelay_neolink_core`'s `pcap_decode_api::Session` to reassemble + decrypt the
//! Bc message stream, and prints the decoded XML for each message.
//!
//! Used to identify Bc message IDs and XML schemas bairelay does not
//! yet model.
//!
//! tshark is required at runtime (Wireshark's CLI). It handles both
//! classic pcap and pcapng input transparently and gives the raw UDP
//! payload bytes per packet, sparing the tool any pcap-format parsing.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use log::{Log, Metadata, Record};
use bairelay_neolink_core::pcap_decode_api::{Credentials, DecodedMessage, Direction, Session};

const USAGE: &str = "\
decode-bc-pcap — offline decoder for captured Baichuan-over-UDP sessions.

USAGE:
    decode-bc-pcap <pcap> <camera-ip>[:port] [--user <username>] \\
                   [--filter <msg_id>[,<msg_id>...]] [--brief]

ARGS:
    <pcap>             Path to a tcpdump capture (pcap or pcapng).
    <camera-ip>        Camera IP, optionally with :port (default: any port
                       observed for that IP). Datagrams to/from this
                       endpoint are demuxed C2D / D2C.

FLAGS:
    --user <name>      Username (default: admin).
    --filter <ids>     Comma-separated msg_id list — print only these.
    --brief            Hide the parsed-struct Debug dump; show only the
                       raw decrypted XML (cleanest output for diffing).

CREDENTIALS:
    The camera password is read from the BAIRELAY_DECODE_PASSWORD env var.
    Never pass it on the command line — it would land in shell history
    and process listings. Typical invocation:

        BAIRELAY_DECODE_PASSWORD='...' \\
          cargo run --manifest-path tests/scripts/decode-bc-pcap/Cargo.toml -- \\
          tests/logs/real-pcap/settime-bairelay.pcap 192.168.x.x:26503

REQUIRES:
    tshark on PATH (Wireshark CLI). The tool spawns it to extract UDP
    payloads from the capture; tshark handles both pcap and pcapng.
";

struct Args {
	pcap: String,
	camera_ip: String,
	camera_port: Option<u16>,
	username: String,
	password: String,
	filter: Option<Vec<u32>>,
	brief: bool,
}

const PASSWORD_ENV: &str = "BAIRELAY_DECODE_PASSWORD";

fn parse_args() -> Result<Args, String> {
	let mut positional: Vec<String> = Vec::new();
	let mut username = String::from("admin");
	let mut filter: Option<Vec<u32>> = None;
	let mut brief = false;
	let mut iter = std::env::args().skip(1);
	while let Some(arg) = iter.next() {
		match arg.as_str() {
			"-h" | "--help" => {
				print!("{USAGE}");
				std::process::exit(0);
			}
			"--user" => {
				username = iter.next().ok_or("--user requires a value")?.to_string();
			}
			"--filter" => {
				let v = iter.next().ok_or("--filter requires a value")?;
				let ids: Result<Vec<u32>, _> =
					v.split(',').map(|s| s.trim().parse::<u32>()).collect();
				filter = Some(ids.map_err(|e| format!("--filter: {e}"))?);
			}
			"--brief" => brief = true,
			s if s.starts_with('-') => return Err(format!("unknown flag: {s}")),
			_ => positional.push(arg),
		}
	}
	if positional.len() != 2 {
		return Err(format!(
			"expected 2 positional args <pcap> <camera-ip[:port]> (got {}). Run with --help.",
			positional.len()
		));
	}
	let pcap = positional.remove(0);
	let cam = positional.remove(0);
	let password = std::env::var(PASSWORD_ENV).map_err(|_| {
		format!(
			"{PASSWORD_ENV} not set. The password must be passed via env var, \
			 not on the command line — see --help."
		)
	})?;

	let (ip, port) = match cam.rsplit_once(':') {
		Some((host, p)) => (
			host.to_string(),
			Some(p.parse::<u16>().map_err(|e| format!("bad port: {e}"))?),
		),
		None => (cam, None),
	};

	Ok(Args {
		pcap,
		camera_ip: ip,
		camera_port: port,
		username,
		password,
		filter,
		brief,
	})
}

/// Captures `log::trace!` records from bairelay_neolink_core's BcCodex and
/// surfaces them via a side channel: the most recent payload XML
/// (per-thread). The decoder pulls this back after each Bc decode call
/// to attach the raw decrypted XML to the corresponding output line.
struct CaptureLogger {
	last_payload: Mutex<Option<String>>,
	last_extension: Mutex<Option<String>>,
}

impl CaptureLogger {
	fn new() -> Self {
		Self {
			last_payload: Mutex::new(None),
			last_extension: Mutex::new(None),
		}
	}

	fn take_payload(&self) -> Option<String> {
		self.last_payload.lock().unwrap().take()
	}
	fn take_extension(&self) -> Option<String> {
		self.last_extension.lock().unwrap().take()
	}
}

impl Log for CaptureLogger {
	fn enabled(&self, _: &Metadata) -> bool {
		true
	}
	fn log(&self, record: &Record) {
		let msg = format!("{}", record.args());
		// bairelay_neolink_core's de.rs format strings:
		//   "Extension Txt: {:?}"
		//   "Payload Txt: {:?}"
		// where `{:?}` Debug-formats a `String` (so the inner value is
		// "..."-quoted). Strip the leading prefix + outer quotes.
		if let Some(rest) = msg.strip_prefix("Payload Txt: ") {
			*self.last_payload.lock().unwrap() = Some(unquote_debug_string(rest));
		} else if let Some(rest) = msg.strip_prefix("Extension Txt: ") {
			*self.last_extension.lock().unwrap() = Some(unquote_debug_string(rest));
		}
		// Other trace records (Encoding/Decoding chatter from the
		// codex layers) are dropped — they're not informative here.
	}
	fn flush(&self) {}
}

/// Convert the Debug-format of a Rust `String` (e.g. `"<?xml ...>"`) back
/// to its content. Handles the quoting + the small set of escapes Rust's
/// default Debug emits (`\n`, `\t`, `\\`, `\"`).
fn unquote_debug_string(s: &str) -> String {
	let s = s.trim();
	let inner = s.strip_prefix('"').unwrap_or(s);
	let inner = inner.strip_suffix('"').unwrap_or(inner);
	let mut out = String::with_capacity(inner.len());
	let mut chars = inner.chars();
	while let Some(c) = chars.next() {
		if c == '\\' {
			match chars.next() {
				Some('n') => out.push('\n'),
				Some('t') => out.push('\t'),
				Some('r') => out.push('\r'),
				Some('"') => out.push('"'),
				Some('\\') => out.push('\\'),
				Some(other) => {
					out.push('\\');
					out.push(other);
				}
				None => out.push('\\'),
			}
		} else {
			out.push(c);
		}
	}
	out
}

static LOGGER: std::sync::OnceLock<&'static CaptureLogger> = std::sync::OnceLock::new();

fn main() {
	let args = parse_args().unwrap_or_else(|e| {
		eprintln!("error: {e}\n\n{USAGE}");
		std::process::exit(2);
	});

	// Install the capture logger before any Bc::deserialize call.
	let captured: &'static CaptureLogger = Box::leak(Box::new(CaptureLogger::new()));
	let _ = LOGGER.set(captured);
	log::set_logger(captured).expect("logger init");
	log::set_max_level(log::LevelFilter::Trace);

	if let Err(e) = run(args, captured) {
		eprintln!("error: {e}");
		std::process::exit(1);
	}
}

fn run(args: Args, log: &'static CaptureLogger) -> Result<(), String> {
	// tshark `-Y` is a display filter (Wireshark syntax), not BPF.
	let bpf = match args.camera_port {
		Some(p) => format!(
			"udp and ip.addr == {} and udp.port == {}",
			args.camera_ip, p
		),
		None => format!("udp and ip.addr == {}", args.camera_ip),
	};
	let mut child = Command::new("tshark")
		.arg("-r")
		.arg(&args.pcap)
		.arg("-Y")
		.arg(&bpf)
		.arg("-T")
		.arg("fields")
		.arg("-e")
		.arg("frame.time_relative")
		.arg("-e")
		.arg("ip.src")
		.arg("-e")
		.arg("udp.srcport")
		.arg("-e")
		.arg("ip.dst")
		.arg("-e")
		.arg("udp.dstport")
		.arg("-e")
		.arg("udp.payload")
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.map_err(|e| {
			format!(
				"failed to spawn tshark: {e}. Is Wireshark/tshark installed and on PATH?"
			)
		})?;

	let stdout = child
		.stdout
		.take()
		.ok_or_else(|| "tshark stdout was not piped".to_string())?;
	let reader = BufReader::new(stdout);

	let creds = Credentials {
		username: args.username.clone(),
		password: Some(args.password.clone()),
	};
	let mut session = Session::new(creds);
	let mut total = 0usize;
	let mut decoded = 0usize;

	for line in reader.lines() {
		let line = line.map_err(|e| format!("read tshark stdout: {e}"))?;
		let parts: Vec<&str> = line.split('\t').collect();
		if parts.len() < 6 {
			continue;
		}
		let ts: f64 = parts[0].parse().unwrap_or(0.0);
		let src = parts[1];
		let _src_port = parts[2];
		let dst = parts[3];
		let dst_port = parts[4];
		let hex = parts[5];
		if hex.is_empty() {
			continue;
		}
		let bytes = match decode_hex(hex) {
			Some(b) => b,
			None => continue,
		};

		let dir = if dst == args.camera_ip {
			Direction::ClientToCamera
		} else if src == args.camera_ip {
			Direction::CameraToClient
		} else {
			continue;
		};
		if let Some(p) = args.camera_port {
			let cam_port_in_pkt = match dir {
				Direction::ClientToCamera => dst_port,
				Direction::CameraToClient => parts[2],
			};
			if cam_port_in_pkt.parse::<u16>().unwrap_or(0) != p {
				continue;
			}
		}

		total += 1;

		// Drain any stale captured XML from before this datagram so
		// the per-message attachment in the closure starts clean.
		let _ = log.take_payload();
		let _ = log.take_extension();

		// Per-message closure — called by Session AFTER each
		// Bc::deserialize completes. This is the only place we can
		// drain the trace logger between successive decodes; if we
		// collected all messages first and drained later, the global
		// last-payload state would be the LAST message's, attributed
		// to the FIRST.
		let filter = args.filter.clone();
		let brief = args.brief;
		let mut local_decoded = 0usize;
		let res = session.feed_datagram(dir, &bytes, |msg| {
			let payload_xml = log.take_payload();
			let extension_xml = log.take_extension();
			if let Some(filter) = &filter {
				if !filter.contains(&msg.bc.meta.msg_id) {
					return;
				}
			}
			local_decoded += 1;
			print_message(ts, &msg, payload_xml, extension_xml, brief);
		});
		decoded += local_decoded;
		if let Err(e) = res {
			eprintln!("[t={ts:.3}s] decode error: {e}");
		}
	}

	let status = child.wait().map_err(|e| format!("tshark wait: {e}"))?;
	if !status.success() {
		let mut stderr = String::new();
		if let Some(mut s) = child.stderr.take() {
			let _ = std::io::Read::read_to_string(&mut s, &mut stderr);
		}
		return Err(format!(
			"tshark exited with {status}. stderr: {}",
			stderr.trim()
		));
	}

	eprintln!(
		"\n--- Summary: scanned {total} datagrams, decoded {decoded} Bc messages ---"
	);
	Ok(())
}

/// Print bytes as hexdump — 16 bytes per line, hex on the left, printable
/// ASCII on the right. Same shape as `xxd` / `hexdump -C`.
fn print_hex_ascii(buf: &[u8]) {
	for (i, chunk) in buf.chunks(16).enumerate() {
		let off = i * 16;
		let mut hex = String::with_capacity(48);
		let mut ascii = String::with_capacity(16);
		for b in chunk {
			hex.push_str(&format!("{:02x} ", b));
			ascii.push(if b.is_ascii_graphic() || *b == b' ' {
				*b as char
			} else {
				'.'
			});
		}
		println!("  {off:08x}  {hex:<48}  |{ascii}|");
	}
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
	let cleaned: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
	if !cleaned.len().is_multiple_of(2) {
		return None;
	}
	let mut out = Vec::with_capacity(cleaned.len() / 2);
	let bytes = cleaned.as_bytes();
	for i in (0..bytes.len()).step_by(2) {
		let hi = (bytes[i] as char).to_digit(16)? as u8;
		let lo = (bytes[i + 1] as char).to_digit(16)? as u8;
		out.push((hi << 4) | lo);
	}
	Some(out)
}

fn print_message(
	ts: f64,
	msg: &DecodedMessage,
	payload_xml: Option<String>,
	extension_xml: Option<String>,
	brief: bool,
) {
	let dir = match msg.direction {
		Direction::ClientToCamera => "C2D",
		Direction::CameraToClient => "D2C",
	};
	let m = &msg.bc.meta;
	println!(
		"=== t={:.3}s {} msg_id={} msg_num={} response_code=0x{:04x} channel_id={} class=0x{:04x} ===",
		ts, dir, m.msg_id, m.msg_num, m.response_code, m.channel_id, m.class
	);
	if let Some(ext) = extension_xml {
		println!("--- Extension XML ---\n{ext}");
	}
	if let Some(payload) = payload_xml {
		println!("--- Payload XML (raw decrypted, before serde parse) ---");
		println!("{payload}");
	} else {
		// Either body had no payload, or the payload was binary, or trace
		// capture missed it (shouldn't happen for XML payloads).
		use bairelay_neolink_core::bc::model::BcBody;
		match &msg.bc.body {
			BcBody::ModernMsg(modern) => match &modern.payload {
				Some(bairelay_neolink_core::bc::xml::BcPayloads::Binary(b)) => {
					println!("--- Payload binary, {} bytes ---", b.len());
					print_hex_ascii(b);
					// Surface a UTF-8 XML view if the raw wire bytes are
					// already plaintext (which is how some msg_ids — e.g.
					// stream chunks in FullAes mode — are sent).
					if let Ok(s) = std::str::from_utf8(b) {
						if s.trim_start().starts_with("<?xml") {
							println!("--- Payload (binary blob is UTF-8 XML) ---");
							println!("{s}");
						}
					}
					// FullAes-on-the-wire case: the production codec
					// returned wire bytes for `(FullAes, no encryptLen)`
					// because it can't tell ciphertext from plaintext
					// without that hint. The Session attached the
					// would-be-plaintext view; print it if it's text-like.
					if let Some(plain) = &msg.manually_decrypted_binary {
						if let Ok(s) = std::str::from_utf8(plain) {
							if s.trim_start().starts_with("<?xml") {
								println!(
									"--- Payload (manually decrypted FullAes view, {} bytes) ---",
									plain.len()
								);
								println!("{s}");
							}
						}
					}
				}
				Some(bairelay_neolink_core::bc::xml::BcPayloads::BcXml(_)) => {
					if !brief {
						println!("--- Payload (parsed only, raw not captured) ---");
						println!("{:#?}", modern.payload);
					}
				}
				None => println!("--- (no payload) ---"),
			},
			other => println!("--- non-modern body: {other:?} ---"),
		}
	}
	println!();
}
