# decode-bc-pcap — offline Baichuan-over-UDP session decoder

Replay a tcpdump capture of a Reolink Baichuan-over-UDP session through `bairelay_neolink_core`'s parsers + AES-CFB primitives, printing each Bc message's header + decrypted payload (XML or binary). Use it to identify Bc message IDs and XML schemas bairelay does not yet model.

## Status

Standalone cargo project, excluded from the workspace. Compiled on demand. `bairelay_neolink_core` is opted into the `pcap-decode-api` feature only when this tool builds — production builds never see the decoder surface.

## Requirements

- **Rust toolchain** — uses the workspace Rust edition.
- **`tshark` on PATH** — Wireshark's CLI. Used to extract per-packet UDP payloads from the capture (handles both `pcap` and `pcapng`). On macOS: `brew install --cask wireshark`.
- **The camera's password** — needed to derive the AES-CFB key once the captured login response selects an AES variant. Pass via the `BAIRELAY_DECODE_PASSWORD` environment variable, never on the command line.

## Usage

```
BAIRELAY_DECODE_PASSWORD='...' \
  cargo run --manifest-path tests/scripts/decode-bc-pcap/Cargo.toml --quiet -- \
  <pcap-path> <camera-ip>[:port] [--user <username>] \
  [--filter <msg_id>[,<msg_id>...]] [--brief] [--tcp]
```

### Arguments

| Arg | Meaning |
|-----|---------|
| `<pcap-path>` | tcpdump capture file (pcap or pcapng). |
| `<camera-ip>` | Camera IP, optionally with `:port`. Datagrams to/from this endpoint are demuxed C2D / D2C. Default port: any. |

### Flags

| Flag | Meaning |
|------|---------|
| `--user <name>` | Camera username. Default: `admin`. |
| `--filter <ids>` | Comma-separated `msg_id` list — print only these messages. Useful with chatty captures where live preview pulls drown out control traffic. |
| `--brief` | Hide the parsed-struct Debug dump; print only header + raw decrypted payload. Cleanest output for diffing C2D vs D2C captures. |
| `--tcp` | Decode Baichuan-over-**TCP** (default port 9000) instead of UDP-wrapped BcUdp. Always-on cameras and the official desktop client speak raw Bc over TCP; the Bc frames ride the TCP byte stream with no BcUdp wrapper. Retransmitted / out-of-order segments are dropped by `tcp.seq` before reassembly. |

### Credentials

`BAIRELAY_DECODE_PASSWORD` env var. Never put the password on the command line — it would land in shell history and `ps aux`. The tool refuses to run without the env var set.

## Output format

Per Bc message (chronological, in-order per direction):

```
=== t=<seconds> <C2D|D2C> msg_id=<n> msg_num=<n> response_code=0x<hex> channel_id=<n> class=0x<hex> ===
--- Extension XML ---
<decrypted Extension block, if present>

--- Payload XML (raw decrypted, before serde parse) ---
<decrypted payload, when payload is XML>

--- Payload binary, <N> bytes ---
  <hexdump + ASCII>
--- Payload (binary blob is UTF-8 XML) ---
<XML view, when the binary blob is actually XML — some replies carry
 textual payloads with `binaryData=1` set>
```

The "raw decrypted, before serde parse" view is load-bearing: it includes XML elements that `BcXml` does not model, which a parsed-struct view would silently drop.

## How it works

The tool pipes `tshark`'s extracted UDP payloads (one packet per line, hex-encoded `udp.payload` field) into `bairelay_neolink_core::pcap_decode_api::Session`. The session:

1. Per direction, reassembles `BcUdp::Data` packets in `packet_id` order.
2. Drives `Bc::deserialize` on the assembled stream — production parser, production decryption.
3. Tracks encryption-protocol negotiation across login (`msg_id=1`, `response_code >> 8 == 0xdd`) — same logic the live `BcCodex` runs.
4. Calls a per-message callback so the tool can grab `log::trace!("Payload Txt: ...")` output between successive decodes (the trace channel is a global; collecting all decodes first and draining later attributes the last message's trace to the first).

`bairelay_neolink_core` exposes the minimum surface needed via the `pcap-decode-api` Cargo feature — `Session`, `Direction`, `DecodedMessage`, `Credentials`, `Error`. None of those compile into release builds.

## Caveats

- **C2D login decode failures are normal.** The first C2D `msg_id=1` is sent before the camera's challenge negotiates the encryption mode. The session starts at `Unencrypted`; the C2D login request was actually sent BCEncrypt. The error logs to stderr and the tool keeps going. The D2C side decodes correctly (the camera's response carries the encryption mode in its header).
- **Some msg_ids may show empty bodies** even when they had content on the wire. This happens when `<encryptLen>` is absent from the Extension and the camera's clone-from-IV decryption gives garbage that doesn't parse. The hexdump still shows the decrypted bytes; you can manually inspect them.
- **AES-CFB keys are derived from username + password + nonce.** Wrong username gives garbage decryption. Default is `admin`; override with `--user`.

## Adding new pcaps

Captures live under `tests/logs/real-pcap/` (gitignored). Naming: `<topic>-<source>.pcap` — e.g. `settime-bairelay.pcap`. Capturing from a router's `tcpdump` is fine for inter-subnet traffic; for intra-subnet phone↔camera traffic, capture from a node positioned to see it (Mac-as-bridge, or run the official desktop client on the Mac so capturing on its `en0` works).

## Protocol facts

Bc message IDs are catalogued in `crates/core/src/bc/model.rs` (the `MSG_ID_*` constants); their XML schemas live in `crates/core/src/bc/xml.rs` (the structs referenced by `BcXml`'s fields, each with a doc comment describing wire semantics). Add new findings there.
