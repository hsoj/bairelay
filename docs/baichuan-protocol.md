# The Baichuan (BC) Protocol — Reverse-Engineered Specification

Reolink's proprietary camera-control protocol, reconstructed from packet captures against live hardware and cross-checked against the independent client implementations listed in §14. **Reolink has published nothing about this protocol.** Every statement below is derived from observed traffic or from decompiled/inferred client behaviour.

Scope: what a client needs to speak BC to a Reolink camera. It covers the three independent framings — **BC** (control, TCP 9000), **BcUdp** (P2P transport + discovery), **BcMedia** (media substream) — plus the authentication suite and the cloud-side wake protocol.

This document specifies the protocol, not any implementation of it. Where implementations are named it is as evidence for a claim or as a record of where they disagree, never as a normative reference.

---

## Confidence notation

Each claim carries one of:

| Mark | Meaning |
|------|---------|
| **[V]** | Verified — confirmed against live hardware, or pinned by a byte-exact packet capture |
| **[O]** | Observed — seen on the wire, semantics inferred from context |
| **[I]** | Inferred — reconstructed from client behaviour or decompilation, not directly confirmed |
| **[?]** | Unknown — field exists, purpose undetermined |

---

## Table of contents

- [1. Protocol overview](#1-protocol-overview)
- [2. BC framing (TCP 9000)](#2-bc-framing-tcp-9000)
- [3. Encryption suite](#3-encryption-suite)
- [4. Authentication](#4-authentication)
- [5. Message catalogue](#5-message-catalogue)
- [6. XML payload vocabulary](#6-xml-payload-vocabulary)
- [7. BcMedia framing](#7-bcmedia-framing)
- [8. BcUdp transport](#8-bcudp-transport)
- [9. P2P discovery and wake](#9-p2p-discovery-and-wake)
- [10. Session lifecycle](#10-session-lifecycle)
- [11. Constants appendix](#11-constants-appendix)
- [12. Robustness requirements](#12-robustness-requirements)
- [13. Open questions](#13-open-questions)
- [14. Provenance](#14-provenance)

---

## 1. Protocol overview

### 1.1 Three framings, one protocol family

Baichuan is not one wire format but three, nested:

```
              ┌─────────────────────────────────────────┐
  TCP 9000    │  BC message  (header + extension + body)│
              └─────────────────────────────────────────┘
                                 │
                    body may be XML or binary
                                 │
                                 ▼
              ┌─────────────────────────────────────────┐
  msg_id 3    │  BcMedia frames (info / I / P / AAC /    │
  binary body │  ADPCM), concatenated in the byte stream │
              └─────────────────────────────────────────┘

              ┌─────────────────────────────────────────┐
  UDP         │  BcUdp: Discovery | Data | Ack          │
              │  Data payloads reassemble into BC bytes │
              └─────────────────────────────────────────┘
```

- **BC** is the control protocol. Everything — login, PTZ, battery, stream start — is a BC message. **[V]**
- **BcMedia** is *not* a transport. It is the format of the binary body of a video message (`msg_id` 3); frames arrive back-to-back in the reassembled byte stream and must be parsed as a stream, not per-BC-message. **[V]**
- **BcUdp** is an alternate carrier for the *same* BC byte stream, used when the client reaches the camera over P2P instead of a direct TCP connect. It adds its own sequencing, ack and retransmit layer. Discovery packets (a third BcUdp variant) carry XML instead. **[V]**

### 1.2 Endianness and text

All integer fields in all three framings are **little-endian**. XML payloads are UTF-8, and are XOR- or AES-obfuscated (§3) rather than plaintext. **[V]**

### 1.3 Default ports

| Port | Proto | Role |
|------|-------|------|
| 9000 | TCP | BC control + media. The "port 9000 protocol". **[V]** |
| 2015, 2018 | UDP | LAN discovery broadcast targets (`C2D_S`, `C2D_C`) **[V]** |
| 3000 | UDP | Legacy: camera replies here to a `C2D_S` broadcast **[O]** |
| 9999 | UDP | P2P "middleman" — UID→register-address lookup **[V]** |
| 58200 | UDP | P2P "register" — heartbeats, wake, session setup **[V]** |
| 57850 | UDP | P2P log/telemetry. Advertised by the cloud; never observed in use **[O]** |
| 443 | TCP | Motion push (`pushx.reolink.com`). TLS with a pinned chain **[O]** |

A camera may be configured with a non-standard BC port; clients should try the configured port and fall back to 9000. **[V]**

---

## 2. BC framing (TCP 9000)

### 2.1 Header

Fixed 20 bytes, plus a conditional 4-byte trailer:

```
offset  size  field            notes
  0       4   magic            0x0abcdef0 (or 0x0fedcba0, §2.2)
  4       4   msg_id           command identifier (§5)
  8       4   body_len         bytes following the header
 12       1   channel_id       0 on standalone cameras; NVR channel otherwise
 13       1   stream_type      0 = clear/main, 1 = fluent/sub, 4 = balanced
 14       2   msg_num          request/reply correlation tag
 16       2   response_code    0 on request; 200/4xx on reply; 0xdcXX/0xddXX during login
 18       2   class            dictates header length + body dialect (§2.3)
 20       4   payload_offset   ONLY when class ∈ {0x6414, 0x0000}
```

Total header length is therefore **20 bytes** for classes `0x6514` / `0x6614` and **24 bytes** for `0x6414` / `0x0000`. **[V]**

> **Two readings of bytes 12–15.** The neolink Wireshark dissector models them as a single 32-bit "XML encryption offset" decomposed as `channel_id | stream_id | reserved(00) | message_handle`. Other implementations read bytes 14–15 as a little-endian `msg_num`. The two agree in the common case: the reserved byte is the low half of `msg_num` and is `00` whenever the handle fits in a byte. Cameras that issue handles ≥ 256 (B800 `subStream` = 256, `externStream` = 1024) make the `msg_num` reading the correct one, and it is the one specified here. **[V]**

Byte 12 (`channel_id`) doubles as the **encryption offset** fed to the BCEncrypt keystream (§3.2) — a single field with two jobs. **[V]**

### 2.2 Magic

`MAGIC_HEADER = 0x0abcdef0`, on the wire as `f0 de bc 0a`. **[V]**

A byte-reversed magic `0x0fedcba0` (`a0 cb ed 0f`) appears on some replies — notably `snap` JPEG payloads — with every other header field still little-endian. It appears to be a hint about *binary payload* endianness rather than header endianness. Every surveyed client ignores the distinction and accepts both magics identically. **[O]**

### 2.3 Message classes

`class` selects both the header length and the body dialect:

| Class | Header | Body | Where it appears |
|-------|--------|------|------------------|
| `0x6514` | 20 B | **Legacy** | The initial login message only |
| `0x6614` | 20 B | Modern | The camera's reply to a `0x6514` login (carries `<Encryption>`) |
| `0x6414` | 24 B | Modern | The generic modern class — most commands |
| `0x0000` | 24 B | Modern | Most modern replies; **and** the sigV3 login request (§4.4) |

A message is "modern" iff `class != 0x6514`. **[V]**

The `0x6414` vs `0x0000` distinction is load-bearing for exactly one case: the camera routes a `0x6414` login into the legacy/modern-encryption handler and a `0x0000` login into the sigV3 token handler. Sending a sigV3 login as `0x6414` yields `417`. **[V]**

### 2.4 Modern body layout

```
┌───────────────────────────┬──────────────────────────────────┐
│  Extension                │  Payload                         │
│  bytes [0, payload_offset)│  bytes [payload_offset, body_len)│
└───────────────────────────┴──────────────────────────────────┘
```

Three degenerate cases, all legal: **[V]**

| `payload_offset` | `body_len` | Meaning |
|---|---|---|
| absent (20-B class) | > 0 | Whole body is payload; no extension |
| 0 | > 0 | No extension (already negotiated); whole body is payload |
| = `body_len` | > 0 | Extension only, no payload |
| 0 | 0 | Header-only message — a bare ack. Check `response_code` |

**Extension** is always XML (`<Extension>`, §6.2) and describes the payload that follows: which channel, whether the payload is binary, how much of it is encrypted.

**Payload** is XML (`<body>`, §6.1) or opaque binary. There is no type flag in the header; a receiver decides by:

1. `Extension.binaryData == 1` → binary, and the `msg_num` enters "binary mode"; **or**
2. the `msg_num` is already in binary mode from a previous message in the same exchange; **otherwise**
3. attempt XML parse.

Binary mode is *sticky per `msg_num`* and is cleared by an extension with `binaryData == 0`. This is how a multi-packet video or snapshot transfer signals framing once and then streams. **[V]**

### 2.5 Legacy body layout

Only `msg_id = 1` (login) has a defined legacy body. It is a fixed 1836 bytes: **[V]**

```
offset  size  field
   0     32   username   uppercase-hex MD5, 31 chars + NUL
  32     32   password   uppercase-hex MD5, 31 chars + NUL  (or 32 NULs if empty)
  64   1772   zero padding
```

The 31-character truncation is not a hash property — it is the firmware's 32-byte buffer with a mandatory NUL terminator, so the last hex nibble of the MD5 is simply discarded. Clients must reproduce the truncation exactly. **[V]**

A special legacy variant, `LoginUpgrade`, carries **no body at all** (header only, `body_len = 0`) and is the modern client's way of asking the camera to skip straight to nonce negotiation without ever transmitting the plain credential MD5s. Every modern client uses this. **[V]**

### 2.6 Correlation and multiplexing

- `msg_num` is a client-allocated tag. The camera echoes it in the reply. Multiple exchanges are in flight concurrently over the single TCP connection, demultiplexed on `(msg_id, msg_num)`. **[V]**
- A response too large for one message is split across several messages **sharing the same `msg_num`** (video, talk-back, firmware upload, snapshots). **[V]**
- Cameras also originate unsolicited messages — motion (`33`), battery (`252`), floodlight status (`291`), UDP keepalive (`234`). These arrive on `msg_num` values the client never allocated, so a client needs a second dispatch path keyed on `msg_id` alone. **[V]**
- **Snapshot caveat:** the camera answers `109` with an XML reply on the request's `msg_num`, then sends the JPEG chunks on a *fresh* `msg_num`. A client must install an `msg_id`-only subscriber **before** releasing the `msg_num`-scoped one, or it loses the chunks that land in the gap. **[V]**

### 2.7 Response codes

| Code | Meaning |
|------|---------|
| `0` | Request (client → camera) |
| `200` | OK **[V]** |
| `201` | OK, final chunk of a multi-part transfer (snapshot) **[V]** |
| `400` | Bad request — also what a malformed packet returns **[V]** |
| `401` | Auth failure **[V]** |
| `406` | Login method refused — new firmware rejecting the plain-MD5 modern login **[V]** |
| `417` | Expectation failed — e.g. sigV3 login sent with the wrong `class` **[V]** |
| `421` | Service unavailable for this identity — account cameras answer `AbilityInfo` (151) with this **[V]** |
| `0xdcXX` | Client → camera: *highest encryption I will accept* (§3.1) **[V]** |
| `0xddXX` | Camera → client: *the encryption that will be used* (§3.1) **[V]** |

The `0xdc` / `0xdd` values are a deliberate overload of the response-code field during login only, distinguished by the high byte.

---

## 3. Encryption suite

Baichuan encrypts **control messages**, not (historically) the media stream. Newer account-bound firmware breaks that rule — see FullAes below. Nothing here provides authentication or integrity; all four modes are confidentiality-only, and two of them are trivially reversible.

### 3.1 Negotiation

The client's legacy login carries its ceiling in `response_code`: **[V]**

| Client sends | Meaning |
|---|---|
| `0xdc00` | None |
| `0xdc01` | BCEncrypt or none |
| `0xdc12` | AES, BCEncrypt or none |

The camera replies with the selection in the low byte of a `0xddXX` code: **[V]**

| Camera replies | Mode |
|---|---|
| `0xdd00` | Unencrypted |
| `0xdd01` | BCEncrypt |
| `0xdd02` | AES (control only) |
| `0xdd03` | Seen on Argus 2 firmware; meaning undetermined — see §13 |
| `0xdd12` | FullAes (control + media) |

The camera never selects above the client's ceiling, but may select below it. **[V]**

**The login message itself is always sent under BCEncrypt** even when AES has been negotiated — the re-key applies only to the post-login session. This holds for the plain modern login, the `authLogin` login, and the sigV3 login. **[V]**

### 3.2 BCEncrypt

A repeating-key XOR with an 8-byte constant key, additionally XORed with the header's `channel_id` byte: **[V]**

```
XML_KEY = [0x1F, 0x2D, 0x3C, 0x4B, 0x5A, 0x69, 0x78, 0xFF]

plain[i] = cipher[i] ^ XML_KEY[(i + offset) mod 8] ^ (offset & 0xFF)
```

where `offset = channel_id` from the header. Symmetric — encrypt and decrypt are the same operation. This is obfuscation, not encryption; the key is public and the plaintext is known-structure XML. **[V]**

### 3.3 AES-128-CFB

Key derivation binds the password to the login nonce: **[V]**

```
key = uppercase_hex( MD5( nonce + "-" + password ) )[0..16]     as ASCII bytes
iv  = "0123456789abcdef"                                          (constant)
cipher = AES-128-CFB128(key, iv)
```

Three things to note:

- The key is the **ASCII text of the first 16 hex characters** of the digest, not the digest bytes. **[V]**
- The IV is a hard-coded ASCII string baked into firmware. IV reuse across sessions is safe only because the key rotates with the per-login nonce. **[V]**
- CFB is a stream mode: ciphertext length equals plaintext length, no padding. **[V]**

### 3.4 FullAes

Same construction, but the **media stream is encrypted too**. Only the leading `Extension.encryptLen` bytes of each media payload are enciphered; the remainder is plaintext. A client that treats a FullAes session as plain AES gets a control channel that works and a video stream that is garbage. **[V]**

sigV3 sessions (§4.4) are always FullAes, and use an ECDHE-derived key **and IV** rather than the constant IV. **[V]**

### 3.5 BcUdp Discovery XOR

The UDP discovery layer uses a different and unrelated obfuscation — a 32-byte keystream from eight `u32` constants offset by the packet's `tid`: **[V]**

```
UDP_KEY = [0x1f2d3c4b, 0x5a6c7f8d, 0x38172e4b, 0x8271635a,
           0x863f1a2b, 0xa5c6f7d8, 0x8371e1b4, 0x17f2d3a5]

keystream = concat( le_bytes(k wrapping_add tid) for k in UDP_KEY )   // 32 bytes
plain[i] = cipher[i] ^ keystream[i mod 32]
```

Wrapping addition is mandatory: any `tid ≥ 0x60000000` overflows, and `tid` is a random `u32`. **[V]**

---

## 4. Authentication

Four login flows exist in the wild. A client must implement at least the first two to cover current hardware.

### 4.1 Legacy → modern (the common case)

```
Client                                          Camera
  │                                               │
  │  msg 1, class 0x6514, LoginUpgrade            │
  │  response_code = 0xdc12  (ceiling)            │
  ├──────────────────────────────────────────────►│
  │                                               │
  │  msg 1, class 0x6614, response 0xdd02         │
  │  <Encryption><nonce>…</nonce>…</Encryption>   │
  │◄──────────────────────────────────────────────┤
  │                        (session re-keys here) │
  │  msg 1, class 0x6414                          │
  │  <LoginUser><userName>md5(user+nonce)</…>     │
  │             <password>md5(pass+nonce)</…>     │
  │             <userVer>1</userVer></LoginUser>  │
  │  <LoginNet><type>LAN</type><udpPort>0</…>     │
  ├──────────────────────────────────────────────►│
  │                                               │
  │  msg 1, class 0x0000, response 200            │
  │  <DeviceInfo>…</DeviceInfo>                   │
  │◄──────────────────────────────────────────────┤
```

Both MD5s are uppercase hex, **truncated to 31 characters** (§2.5). The nonce prevents replay but not rainbow-table attacks — and the legacy path, when a client actually sends credentials rather than `LoginUpgrade`, has already leaked the unsalted MD5s. **Password strength is the only real defence here.** **[V]**

A `200` reply with an *empty* body is a rejection, not a success — treat it as an auth failure. **[V]**

### 4.2 `<Encryption>` reply fields

```xml
<Encryption version="1.1">
  <type>md5</type>
  <nonce>0-AhnEZyUg6eKrJFIWgXPF</nonce>
  <authTypeList>
    <authType>password</authType>
    <authType>sigV1</authType>
    <authType>sigV3</authType>
    <authType>authLogin</authType>
    <authType>getAccesskey</authType>
  </authTypeList>
  <sigVer>v3</sigVer>
  <ECDHE>
    <pubKeyAlgo>X25519</pubKeyAlgo>
    <publicKey>…base64 32 bytes…</publicKey>
    <pubKeySign>…base64…</pubKeySign>
    <iterations>1000</iterations>
  </ECDHE>
</Encryption>
```

`authTypeList`, `sigVer` and `ECDHE` are absent on older firmware. Nonce format varies by model — older cameras emit 16 hex characters (`9E6D1FCB9E69846D`), newer ones a 21-character mixed-alphabet string. Treat it as an opaque string. **[V]**

**Advertisement is not a requirement.** A camera listing `sigV3` and `getAccesskey` will still accept the plain modern login on most firmware. A client should only escalate when the plain login is *refused* (`406`), or when it is deliberately operating as an account-bound client. **[V]**

### 4.3 `authLogin` — camera-local challenge-response

For new-firmware battery cameras that reject the plain login and advertise `getAccesskey`. Needs no cloud account. **[I]** (reconstructed from the official app's `authCodeLogin` state machine; verified end-to-end against a mock, not yet on hardware)

```
1.  → LoginUser{ authType="getAccesskey",
                 AuthInfo{ authCode = md5(password + nonce)[0..31] } }

2.  ← binary payload, 128 bytes:
        [0x00 .. 0x40)  base64 token A, NUL-padded
        [0x40 .. 0x80)  base64 token B, NUL-padded
    Each token, once base64-decoded, is AES-128-CFB under the
    standard make_aeskey(nonce, password) key and constant IV.

3.  → LoginUser{ authType="authLogin",
                 userName = md5(decrypt(A) + nonce)[0..31],
                 password = md5(decrypt(B) + nonce)[0..31],
                 userVer  = 1 }

4.  ← DeviceInfo, response 200
```

The construction proves password knowledge twice — once by `authCode`, once by the ability to decrypt the challenge — but adds no forward secrecy.

### 4.4 sigV3 — ECDHE + cloud token

Required by account-bound ("cloud") cameras. The camera offers an ephemeral X25519 public key; the client answers with its own plus an encrypted proof. **[V]** for the wire shape, **[I]** for the derivation (recovered from `BaichuanDevice::signatureLoginV3`).

```
shared     = X25519(client_priv, camera_pub)          // raw RFC 7748 output, no post-hash
kdf_input  = nonce, zero-padded into a fixed 32-byte buffer
             (first 31 bytes copied, length passed as 32)
derived    = PBKDF2-HMAC-SHA256(kdf_input, salt = shared, iterations, dkLen = 32)
session_key = derived[0..16]
session_iv  = derived[16..32]

cipherContent = base64( AES-128-CFB128(session_key, session_iv,
    {"nonce":"…","clientTime":<unix>,"token":{"p":"…","s":"…"}} ) )
```

The login message is `msg_id 1`, **`class = 0x0000`**, and carries:

| Field | Value |
|-------|-------|
| `userName` | `md5(username + nonce)[0..31]` |
| `password` | `md5("" + nonce)[0..31]` — **the empty string**, not the device password |
| `publicKey` | base64 of the client's X25519 public key |
| `cipherContent` | as above |
| `tokenKey`, `certChain` | from the Reolink cloud token bundle |

Two separate proofs are in play, and it is worth being precise about what each one does:

- The **password** is *not* proven at all on this path. `password` is `md5(nonce)`, computable by anyone. Authentication comes entirely from the cloud-issued token bundle.
- The **ECDHE layer** proves possession of the private key matching the `publicKey` sent — anti-replay and anti-MITM session binding. Both sides derive it from public values, so it proves nothing about identity on its own.

On a `200`, the session immediately switches to **FullAes keyed by `(session_key, session_iv)`** — the reply itself still arrives under BCEncrypt. A client must arm this switch when *encoding* the login and apply it when *decoding* that specific `msg_num`'s reply, disarming on rejection. **[V]**

The `nonce` fed to the KDF is whichever the camera issued for this login: the `nc` field from the P2P handshake (§9.4) on the direct path, or the `<Encryption>` nonce on the negotiated path. Both are equivalent inputs. **[V]**

### 4.5 Handshake-delivered credentials

When a client connects with `lver=3` in its `C2D_C` discovery packet, the camera returns the login nonce (`nc`) and its ECDHE offer (`pl`) inside the *P2P handshake* rather than in a BC `<Encryption>` reply. In that case the sigV3 login is sent **directly** — the camera will not answer a `LoginUpgrade` after an `lver=3` handshake. **[V]**

The `pl` line is a flat `Pn=value` list, split on `,` and `;`:

```
V=1;C=…,P2=v3,P3=X25519,P4=<camera pubkey b64>,P5=<sign b64>,P6=<iterations>;
```

---

## 5. Message catalogue

`msg_id` values. Entries marked ⚑ also appear in the neolink dissector's `messages.md`; the remainder are attested by client implementations and captures but absent from that table.

### 5.1 Session and system

| ID | Name | Direction | Notes |
|----|------|-----------|-------|
| 1 ⚑ | Login | C→D | Legacy and modern; the only `0x6514` message |
| 2 ⚑ | Logout | C→D | |
| 58 ⚑ | `<AbilitySupport>` | C→D | Users + general system info |
| 59 | `<UserList>` update | C→D | Create / update / remove users |
| 80 ⚑ | `<VersionInfo>` | C→D | |
| 93 ⚑ | `<LinkType>` / Ping | C→D | Used as a liveness probe |
| 104 ⚑ | `<SystemGeneral>` read | C→D | Clock, timezone |
| 105 | `<SystemGeneral>` write | C→D | |
| 106 | `<Dst>` read | C→D | DST config; often flagged binary despite being UTF-8 XML |
| 107 | `<Dst>` write | C→D | Symmetric to 106; not exercised in the wild **[I]** |
| 114 | `<Uid>` | C→D | Fetch the camera UID |
| 151 ⚑ | `<AbilityInfo>` | C→D | Per-user ability table |
| 199 ⚑ | `<Support>` | C→D | Feature support (PTZ, talk, …) |
| 234 | UDP keepalive | **D→C** | Camera-initiated; reply `200`, empty body |

### 5.2 Media

| ID | Name | Direction | Notes |
|----|------|-----------|-------|
| 3 ⚑ | Video/audio stream start | C→D | Body `<Preview>`; reply then BcMedia binary |
| 4 ⚑ | Stream stop | C→D | |
| 109 | Snapshot | C→D | XML reply, then JPEG chunks on a new `msg_num` (§2.6) |
| 146 ⚑ | `<StreamInfoList>` | C→D | Available streams + encode table |
| 201 ⚑ | `<TalkConfig>` | C→D | Precedes talk-back data |
| 202 ⚑ | Talk | C→D | Binary audio upload |
| 10 ⚑ | `<TalkAbility>` | C→D | |
| 11 | TalkReset | C→D | |
| 263 | Play audio | C→D | Siren and other stored sounds |

### 5.3 PTZ and optics

| ID | Name | Direction |
|----|------|-----------|
| 18 ⚑ | `<PtzControl>` | C→D |
| 19 | PTZ goto preset | C→D |
| 190 ⚑ | `<PtzPreset>` read | C→D |
| 294 | `<PtzZoomFocus>` read | C→D |
| 295 | Zoom/focus write | C→D |

### 5.4 Alarms, battery, lighting

| ID | Name | Direction | Notes |
|----|------|-----------|-------|
| 31 ⚑ | Start motion alarm | C→D | Subscribes to 33 |
| 33 ⚑ | `<AlarmEventList>` | **D→C** | Motion events |
| 212 | `<rfAlarmCfg>` read (PIR) | C→D | |
| 213 | PIR alarm start / write | C→D | |
| 208 ⚑ | `<LedState>` read | C→D | |
| 209 ⚑ | `<LedState>` write | C→D | |
| 252 | `<BatteryList>` | **D→C** | Camera-initiated (login, low battery) |
| 253 | `<BatteryInfo>` | C→D | Client-initiated poll |
| 288 ⚑ | `<FloodlightManual>` | C→D | |
| 290 ⚑ | `<FloodlightTask>` write | C→D | |
| 291 ⚑ | `<FloodlightStatusList>` | **D→C** | |
| 438 ⚑ | `<FloodlightTask>` read | C→D | |

### 5.5 Network, email, maintenance

| ID | Name | Direction |
|----|------|-----------|
| 23 ⚑ | Reboot | C→D |
| 36 | Set service ports | C→D |
| 37 | Get service ports | C→D |
| 42 | `<Email>` read | C→D |
| 43 | `<Email>` write | C→D |
| 141 | Test email | C→D |
| 216 | `<EmailTask>` write | C→D |
| 217 | `<EmailTask>` read | C→D |
| 124 | `<PushInfo>` | C→D |

### 5.6 Attested but not exercised

From the neolink dissector. Present in firmware, but not exercised by any implementation surveyed for this document, so the payload shapes are unconfirmed: **[O]**

`5–16` file operations · `25/26/78/132` `<VideoInput>` · `44/45` `<OsdChannelName>` · `52/53` `<Shelter>` · `54/55` `<RecordCfg>` · `56/57` `<Compression>` · `67` `<ConfigFileInfo>` (firmware upgrade) · `76/77` `<Ip>` · `79` `<Serial>` · `81/82` `<Record>` schedule · `102` `<HDDInfoList>` · `115` `<WifiSignal>` · `133/204` `<RfAlarm>` · `264` `<audioCfg>`

---

## 6. XML payload vocabulary

### 6.1 Payload document

The payload XML has a root element `<body>` containing exactly one (occasionally more) of the known child elements. Element names are `PascalCase` with a handful of `camelCase` exceptions (`rfAlarmCfg`). Most carry a `version` **attribute** — `1.1` on everything observed. **[V]**

```xml
<?xml version="1.0" encoding="UTF-8" ?>
<body>
  <Preview version="1.1">
    <channelId>0</channelId>
    <handle>0</handle>
    <streamType>mainStream</streamType>
  </Preview>
</body>
```

Deserialisers must be **tolerant**: firmware emits sub-elements that no published client models, and `DeviceInfo` in particular is far larger than any client parses. Unknown elements are dropped, not errors. **[V]**

### 6.2 `<Extension>`

The one element that appears in the extension slot rather than the payload:

| Field | Type | Purpose |
|-------|------|---------|
| `@version` | attr | `1.1` |
| `binaryData` | u32 | `1` → payload is binary and `msg_num` enters binary mode; omitted otherwise |
| `channelId` | u8 | Target channel |
| `userName` | string | Required by `AbilitySupport` — the camera does not infer it from the session **[?]** |
| `token` | string | Comma-separated ability categories: `"system, network, alarm, record, video, image"` |
| `rfId` | u8 | PIR sensor selector |
| `checkPos`, `checkValue` | u32 / i32 | Decryption self-check for encrypted binary **[I]** |
| `encryptLen` | u32 | Bytes of the payload that are AES-encrypted under FullAes (§3.4) |

That the camera needs an explicit `userName` to answer an ability query — rather than using the authenticated session identity — is a plausible authorisation bypass. It has not been tested. **[?]**

### 6.3 `<Preview>` — stream selection

`handle` and `stream_type` disagree across models, which is the single most model-dependent corner of the protocol: **[V]**

| Stream | `stream_type` (header byte 13) | `handle` | `streamType` |
|--------|-------------------------------|----------|--------------|
| Main | 0 | 0 | `mainStream` |
| Sub | 1 | 256 | `subStream` |
| Extern | 0 | 1024 | `externStream` |

On E1 and Swann cameras `handle` is 0 for main and 1 for sub, and `externStream` does not exist. On B800 the header's `stream_type` and `msg_num` are left at 0 by the official client and the `handle` carries the selection instead. Follow the official-client values above; cameras that do not support `externStream` silently serve the sub-stream. **[V]**

### 6.4 Element index

Login and identity — `Encryption`, `AuthTypeList`, `Ecdhe`, `LoginUser`, `AuthInfo`, `LoginNet`, `DeviceInfo`, `VersionInfo`, `Resolution`, `Uid`, `UserList`, `User`, `AbilityInfo`, `Support`

Media — `Preview`, `Snap`, `StreamInfoList`, `StreamInfo`, `EncodeTable`, `StreamResolution`, `TalkConfig`, `TalkAbility`, `AudioConfig`, `AudioPlayInfo`

Control — `PtzControl`, `PtzPreset`, `PresetList`, `Preset`, `PtzZoomFocus`, `LedState`, `FloodlightManual`, `FloodlightTask`, `FloodlightStatusList`

Events and power — `AlarmEventList`, `AlarmEvent`, `RfAlarmCfg`, `TimeBlockList`, `AlarmHandle`, `BatteryList`, `BatteryInfo`

System — `SystemGeneral`, `Norm`, `Dst`, `Email`, `EmailTask`, `ScheduleList`, `ServerPort`, `HttpPort`, `HttpsPort`, `RtspPort`, `RtmpPort`, `OnvifPort`, `PushInfo`, `LinkType`

No implementation surveyed models the complete field set of any of these elements; each parses the subset it needs and tolerates the rest. Treat the list as an index of element names, not a schema.

### 6.5 The DST trap

`<Dst>` (msg 106) is a genuine interoperability hazard. The camera **autonomously applies the DST `<offset>`** to displayed local time whenever the current date falls inside the configured window. A client writing `<SystemGeneral>` must therefore send the **base UTC offset with DST excluded** in `<timeZone>`, and UTC in the wallclock fields — otherwise the camera double-applies DST and the clock runs `offset` hours fast for half the year. **[V]**

Note also that `<timeZone>` is *negative* seconds from UTC: UTC+7 is `-25200`. **[V]**

---

## 7. BcMedia framing

The binary payload of a stream message (`msg_id 3`) is a concatenated sequence of BcMedia frames. Frames do not align to BC message boundaries — a client must reassemble the payloads into one byte stream and parse it independently. **[V]**

Every frame begins with a little-endian 32-bit magic:

| Magic | ASCII | Frame |
|-------|-------|-------|
| `0x31303031` | `1001` | InfoV1 |
| `0x32303031` | `1002` | InfoV2 |
| `0x63643030`–`0x63643039` | `cd00`–`cd09` | I-frame (last digit = channel) |
| `0x63643130`–`0x63643139` | `cd10`–`cd19` | P-frame (last digit = channel) |
| `0x62773530` | `bw50` | AAC |
| `0x62773130` | `bw10` | ADPCM |

### 7.1 Info frames

Identical layout for V1 and V2; the magic is the only difference and no behavioural distinction has been identified. **[?]**

```
offset  size  field
  0       4   magic
  4       4   header_size    always 32
  8       4   video_width
 12       4   video_height
 16       1   unknown        observed 0x00 / 0x01  [?]
 17       1   fps            on older cameras an index into a lookup table, not a rate  [O]
 18       6   start Y,M,D,h,m,s   each u8
 24       6   end   Y,M,D,h,m,s   each u8
 30       2   unknown  [?]
```

Start/end timestamps are only meaningful for SD-card playback. **[O]**

### 7.2 I-frame

```
offset  size  field
  0       4   magic
  4       4   video_type     ASCII "H264" or "H265"
  8       4   payload_size
 12       4   additional_header_size
 16       4   microseconds   presentation timestamp
 20       4   unknown        observed 00 / 23 / 5A  [?]
 24       4   time           POSIX seconds — present iff additional_header_size >= 4
 28     N-4   additional header remainder — present iff additional_header_size > 4  [?]
  …       P   payload  (P = payload_size)
  …     pad   zero padding to an 8-byte boundary of payload_size
```

### 7.3 P-frame

Same as the I-frame minus the POSIX `time` field: the whole `additional_header_size` block is skipped rather than interpreted.

### 7.4 AAC

```
offset  size  field
  0       4   magic
  4       2   payload_size
  6       2   payload_size (repeated, identical)  [?]
  8       P   ADTS-framed AAC
  …     pad   zero padding to an 8-byte boundary of payload_size
```

Frame duration is derived from the ADTS header rather than signalled: `samples = (frame_count_field + 1) * 1024`, `duration_µs = samples * 1e6 / sample_rate`, with `sample_rate` from the 4-bit ADTS frequency index. **[V]**

### 7.5 ADPCM

```
offset  size  field
  0       4   magic
  4       2   payload_size    includes the 4-byte sub-header below
  6       2   payload_size (repeated)
  8       2   sub-magic       0x0100
 10       2   half_block_size — "just 2" on some cameras, half the block on others  [?]
 12   P-4     DVI-4 ADPCM: 4 bytes predictor state, then one block of samples
  …     pad   zero padding to an 8-byte boundary of payload_size
```

Always 8 kHz. `duration_µs = (payload_size - 4) * 2 * 1e6 / 8000`. **[V]**

> **Padding asymmetry.** The wire format pads on `payload_size % 8`, which *includes* the 4-byte sub-header. At least one implementation's serialiser pads on the payload length *excluding* the sub-header, so ADPCM written by it does not round-trip through its own parser. Camera-produced bytes are unaffected; synthetic ADPCM may desync. Serialisers should pad on `payload_size`, matching the parser. **[V]**

### 7.6 Codec notes

- H.264 and H.265 payloads are ordinary Annex-B NAL streams. No Reolink-specific transformation. **[V]**
- Under FullAes only the leading `Extension.encryptLen` bytes of the payload are encrypted (§3.4). **[V]**

---

## 8. BcUdp transport

When the client reaches the camera over P2P instead of a direct TCP connect, the same BC byte stream is chunked into `UdpData` packets with an ack/retransmit layer on top. Three packet types, distinguished by magic:

| Magic | Type |
|-------|------|
| `0x2a87cf3a` | Discovery (XML, §9) |
| `0x2a87cf10` | Data |
| `0x2a87cf20` | Ack |

### 8.1 Discovery

```
offset  size  field
  0       4   magic = 0x2a87cf3a
  4       4   payload_size
  8       4   unknown, always 1
 12       4   tid            transmission id; doubles as the XOR key offset
 16       4   crc32          of the *encrypted* body
 20       N   XOR-encrypted XML  (§3.5)
```

The CRC is a non-standard CRC-32: polynomial `0x04c11db7`, init `0x00000000`, xorout `0x00000000` — i.e. CRC-32/ISO-HDLC with both the initial inversion and the final xorout undone. **[V]**

### 8.2 Data

```
offset  size  field
  0       4   magic = 0x2a87cf10
  4       4   connection_id   (i32 — the register can hand back negative values)
  8       4   unknown, always 0
 12       4   packet_id       monotonic sequence number
 16       4   payload_size
 20       N   raw BC bytes (fragment)
```

`connection_id` is the **peer's** id: `did` when sending to the camera, `cid` when receiving from it. Both are negotiated during discovery. **[V]**

Payloads are BC-message fragments, not whole messages. MTU is 1350 bytes total, so the fragment ceiling is `1350 - 20 = 1330` bytes. Reassemble by `packet_id` order into a byte stream, then parse BC framing from that stream. **[V]**

### 8.3 Ack

```
offset  size  field
  0       4   magic = 0x2a87cf20
  4       4   connection_id (i32)
  8       4   unknown, always 0
 12       4   group_id        0 normally; 0xffffffff = "nothing received yet"
 16       4   packet_id       last contiguously-received packet
 20       4   maybe_latency   changes ~1/s; purpose unconfirmed  [?]
 24       4   payload_size
 28       N   selective-ack bitmap
```

The bitmap is a byte-per-packet truth table (`00 01 01 01 …`) covering packets *after* `packet_id`, marking which have arrived. Unacknowledged packets are retransmitted. **[O]**

Observed official-client timings: **ack every 10 ms**, **retransmit every 500 ms**. **[V]**

### 8.4 Reliability model

Cumulative ack plus a selective bitmap, with sender-side timed retransmit — a hand-rolled reliable stream over UDP. There is no flow control, no congestion control, and no connection teardown beyond the discovery-layer `DISC` verbs. **[O]**

---

## 9. P2P discovery and wake

Two distinct problems: finding an awake camera on the LAN, and waking a sleeping battery camera through Reolink's cloud (or a local replacement).

### 9.1 XML envelope

All discovery payloads are a single element wrapped in `<P2P>`:

```xml
<P2P><C2M_Q><uid>9527000XXXXXXXXX</uid><p>MAC</p></C2M_Q></P2P>
```

Verb names encode direction and role: `C` = client, `D` = device (camera), `M` = middleman, `R` = register. `_R` suffix = reply. So `D2M_Q` is device→middleman query and `M2D_Q_R` is its reply. **[V]**

### 9.2 Verb index

| Verb | From → To | Purpose |
|------|-----------|---------|
| `C2D_S` | client → LAN broadcast :2015 | "Any camera, announce yourself." Camera replies to the given port **[O]** |
| `C2D_C` | client → LAN broadcast :2018 | "Camera with this UID, connect to me" |
| `D2C_C_R` | camera → client | Connect reply: `cid`, `did`, `rsp`; plus `nc`/`pl` when `lver=3` |
| `C2D_T` / `D2C_T` | both | Transport negotiation; `conn` ∈ `local` / `relay` / `map` |
| `D2C_CFM` | camera → client | Session confirm |
| `C2D_A` | client → camera | Accept |
| `C2D_HB` / `D2C_HB` | both | Session keepalive |
| `C2D_DISC` / `D2C_DISC` / `R2C_DISC` | any | Disconnect |
| `C2M_Q` → `M2C_Q_R` | client ↔ middleman :9999 | UID lookup → `reg`/`relay`/`log`/`t` addresses |
| `D2M_Q` → `M2D_Q_R` | camera ↔ middleman :9999 | Camera boot query → `reg`/`log` + `rsp`/`token`/`ac` |
| `D2R_R` → `R2D_R_R` | camera ↔ register :58200 | Session anchor |
| `D2R_HB` → `R2D_HB_R` | camera ↔ register | Steady-state heartbeat, ~10 s |
| `C2R_C` → `R2C_C_R`, `R2C_T` | client ↔ register | Wake request |
| `R2D_C` → `D2R_C_R` | register ↔ camera | The wake packet itself |
| `C2R_CFM`, `C2R_HB` | client → register | Confirm / keepalive |
| `D2R_DISC` → `R2D_DC_R` | camera ↔ register | End-of-session diagnostics |
| `R2C_C_R` | register → client | Register-mediated connect reply |

### 9.3 Two reply shapes that must not be confused

`M2C_Q_R` (to clients) carries four `IpPort` blocks — `reg`, `relay`, `log`, `t` — and no session fields.

`M2D_Q_R` (to cameras) carries two `IpPort` blocks — `reg`, `log` — plus empty `<timer/>` and `<retry/>` marker elements, `<rsp>`, `<token>` and `<ac>`. **Field order matters and the empty markers are required.** A camera silently rejects an `M2D_Q_R` shaped like `M2C_Q_R`. **[V]**

```xml
<P2P><M2D_Q_R>
  <reg><ip>…</ip><port>58200</port></reg>
  <log><ip>…</ip><port>57850</port></log>
  <timer/>
  <retry/>
  <rsp>0</rsp>
  <token>1773137273</token>
  <ac>1130209852</ac>
</M2D_Q_R></P2P>
```

### 9.4 `lver=3` — credentials in the handshake

A client that sets `<lver>3</lver>` in its `C2D_C` gets a `D2C_C_R` carrying `nc` (the login nonce, an i64) and `pl` (the ECDHE offer line, §4.5). The sigV3 login then proceeds directly, without a `LoginUpgrade`. Omit `<lver>` entirely for a legacy connect — do not send `0`. **[V]**

### 9.5 UID forms

Cameras report a **long-form** UID (16-character base + 4-character firmware suffix) in `D2M_Q` / `D2R_HB`; clients and operator configuration use the **short form** (16 characters) in `C2M_Q` / `C2R_C`. A registry keyed on UID must prefix-match, or client wake requests never resolve to the camera's own registration. **[V]**

### 9.6 Battery-camera boot sequence

```
0. Camera boots, joins wifi.
1. DNS: p2p.reolink.com
2. camera → middleman:9999      D2M_Q    {uid (long), r=2}          ~85 B
3. middleman → camera           M2D_Q_R  {reg, log, rsp, token, ac} ~226 B
4. camera → register:58200      D2R_R    {uid, token (echoed), r}   ~110 B
5. register → camera            R2D_R_R  {rsp=-4, ac (echoed)}      ~82 B
6. camera → register  (~10 s)   D2R_HB   {uid, [dev], needrsp, token} ~125 B
7. register → camera            R2D_HB_R {rsp, time_t, timer{hb}}   ~82-180 B
```

Two counter-intuitive details, both verified: **[V]**

- **`<rsp>-4</rsp>` in `R2D_R_R` is informational, not an error.** Cameras accept it and proceed. Reolink's own cloud sends it.
- **The `<ac>` must match** what was issued in `M2D_Q_R`, or the camera silently restarts from `D2M_Q`.

The `<hb>` value in `R2D_HB_R` (observed `20000`) is advisory — cameras pick their own cadence, ~10 s in practice. **[V]**

The `<dev>` block in `D2R_HB` is the camera's *self-reported* LAN address and must **not** be used as the reply target; reply to the packet's actual source address. Some firmware omits `<dev>` entirely. **[V]**

### 9.7 Wake-on-demand

```
1. client → middleman:9999   C2M_Q   {uid (short), p=MAC}
2. middleman → client        M2C_Q_R {reg, relay, log, t}
3. client → register:58200   C2R_C   {uid (short), cli, relay, cid,
                                      debug, family=4, p, r=3}
4. register → camera         R2D_C   {cli, cmap, relay, sid, cid}   × 10 @ 100 ms
5. camera → register         D2R_C_R  (informational)
6. register → client         R2C_C_R + R2C_T {dev, dmap, sid, cid, rsp}
7. camera wakes; client performs the normal BC handshake on TCP 9000
```

Ten fire-and-forget wake packets at 100 ms; cameras typically respond to the first. **This is the only firmware-supported mechanism for waking a sleeping battery camera.** LAN broadcast discovery (2015/2018) finds only cameras that are already awake. **[V]**

### 9.8 Relay hosts

`p2p.reolink.com` and `p2p1..p2p11.reolink.com` (port 9999). Entries 12–16 resolve to `127.0.0.1` on Reolink's side — presumably reserved. Observed deployment: middleman on AWS `eu-west-3`, register and log on Linode. **[O]**

---

## 10. Session lifecycle

A complete client session, direct-TCP case:

```
1.  TCP connect to camera:9000
2.  Login (§4) — establishes the session encryption
3.  Query abilities (msg 151) — advisory; a missing entry does not
    imply the feature is absent, and account cameras answer 421
4.  Register a handler for camera-initiated messages:
      234 keepalive (reply 200, empty), 33 motion,
      252 battery, 291 floodlight status
5.  Issue commands, each on a fresh msg_num
6.  Streaming: msg 3 with <Preview> → 200 → BcMedia byte stream
7.  Stop: msg 4, then logout (msg 2), then close
```

Load-bearing details:

- **`AbilityInfo` is advisory.** A failure must not sink an otherwise-successful login. **[V]**
- **Keepalive is camera-initiated.** The camera sends `234`; the client replies `200` with an empty body. There is no client-initiated keepalive on this path — `ping` (msg 93) is a liveness probe, not a session keepalive. **[V]**
- **Always send stop.** A battery camera that loses its listener without a `msg_id 4` keeps streaming until its own session timeout — minutes of battery, not seconds. **[V]**
- **Auth failures are terminal.** A `401`-class rejection means the credentials are wrong; retrying with backoff just hammers the camera. Connection failures are retryable; auth failures are not. **[V]**

---

## 11. Constants appendix

```
BC (TCP)
  MAGIC_HEADER          0x0abcdef0        wire: f0 de bc 0a
  MAGIC_HEADER_REV      0x0fedcba0        wire: a0 cb ed 0f
  Classes               0x6514 legacy/20B  0x6614 modern/20B
                        0x6414 modern/24B  0x0000 modern/24B
  Legacy login body     1836 bytes (32 user + 32 pass + 1772 zeros)
  MD5 field width       31 uppercase hex chars (+ optional NUL)

Encryption
  XML_KEY (BCEncrypt)   1F 2D 3C 4B 5A 69 78 FF
  AES_IV                "0123456789abcdef"
  AES key               uppercase_hex(MD5(nonce + "-" + password))[0..16] as ASCII
  Enc negotiation       client 0xdc00/0xdc01/0xdc12
                        camera 0xdd00/0xdd01/0xdd02/0xdd12  (0xdd03 seen, §13)

BcUdp
  MAGIC_UDP_NEGO        0x2a87cf3a        Discovery
  MAGIC_UDP_DATA        0x2a87cf10        Data
  MAGIC_UDP_ACK         0x2a87cf20        Ack
  UDP_KEY               1f2d3c4b 5a6c7f8d 38172e4b 8271635a
                        863f1a2b a5c6f7d8 8371e1b4 17f2d3a5
  CRC-32                poly 0x04c11db7, init 0x00000000, xorout 0x00000000
  MTU                   1350 (data fragment ceiling 1330)
  Ack cadence           10 ms      Retransmit cadence 500 ms

BcMedia
  INFO_V1  0x31303031 "1001"      INFO_V2  0x32303031 "1002"
  IFRAME   0x63643030–39 "cd0x"   PFRAME   0x63643130–39 "cd1x"
  AAC      0x62773530 "bw50"      ADPCM    0x62773130 "bw10"
  ADPCM sub-magic 0x0100          Frame padding: 8 bytes
  ADPCM sample rate 8000 Hz

XML
  Version attribute     "1.1"
  LoginNet defaults     type="LAN", udpPort=0
  timeZone units        negative seconds from UTC (UTC+7 → -25200)
```

---

## 12. Robustness requirements

Every length and count in this protocol is attacker-controlled — by a compromised camera, an on-path attacker (nothing here is authenticated), or a hostile peer on the public P2P ports. A conforming parser **must**:

1. **Cap `body_len`.** A header declaring 4 GiB drives a framed reader's buffer toward 4 GiB before any payload validation runs. Observed messages sit far below 8 MiB — a 4K snapshot is ~3 MiB, XML payloads are kilobytes, and large I-frames arrive through BcMedia framing rather than BC — so a ceiling in that region rejects crafted headers without truncating legitimate traffic. **[V]**
2. **Validate `payload_offset <= body_len` before subtracting.** Otherwise the extension-length subtraction underflows — a trap where arithmetic is checked, a wrap to ~4 GiB where it is not, which becomes the same memory exhaustion as (1). **[V]**
3. **Cap BcUdp `payload_size` at 65535.** IPv4 bounds a datagram at 64 KiB, so any larger value is by construction crafted. **[V]**
4. **Clamp `Extension.encryptLen` to the actual buffer length.** It is camera-supplied; an out-of-range value drives an out-of-bounds read. **[V]**
5. **Reject ADPCM `payload_size < 4`.** The sub-header subtraction underflows below that, with the same consequences as (2). **[V]**
6. **Bound sigV3 `iterations`.** Camera-supplied and fed to PBKDF2; `0` breaks the KDF and a large value is a CPU-exhaustion vector. The observed real value is `1000`, so a range such as `1..=1_000_000` bounds both failure modes with ample headroom. **[V]**
7. **Use wrapping arithmetic in the UDP keystream.** Any `tid >= 0x60000000` overflows `u32`. **[V]**
8. **Drop discovery packets that fail CRC** rather than erroring the listener task. Public UDP ports see junk continuously — unrelated traffic, scanners, other Reolink products, firmware variants. A CRC failure or an unknown XML verb is routine, not exceptional. **[V]**
9. **Cap multi-chunk transfers.** The snapshot loop terminates only on a non-200 code or a dropped connection; without a ceiling a buggy camera can exhaust memory. **[V]**

### 12.1 Security properties

Stated plainly, because the protocol's own design does not:

- **No integrity protection anywhere.** No MAC, no signature over messages. The BcUdp discovery CRC detects corruption, not tampering.
- **No transport authentication.** An on-path attacker can modify any BC message, in any encryption mode.
- **BCEncrypt is obfuscation.** Public key, XOR, known-structure plaintext.
- **AES-128-CFB with a constant IV**, keyed by `MD5(nonce + "-" + password)`. The nonce rotation is what keeps IV reuse from being catastrophic.
- **The legacy login leaks unsalted credential MD5s** to anyone watching. `LoginUpgrade` exists to avoid this; use it.
- **Wire dumps contain credential hashes and camera UIDs.** Debug logging of decrypted extension/payload XML must not go to stdout by default.
- **sigV3 proves possession of a cloud token, not of the device password.** Its `<password>` field is `md5(nonce)` — a public value.

---

## 13. Open questions

Unresolved after this round of analysis. Each is a concrete, testable gap.

1. **`0xdd03` encryption mode.** A captured Argus 2 login reply carries `response_code = 0xdd03`, a low byte absent from the negotiation table in §3.1. Either `0x03` names a distinct mode, or it is a BCEncrypt variant. The ambiguity is easy to miss: implementations that treat *any* non-`0x00` low byte as BCEncrypt decode such a session correctly by accident, while those that match the table exactly reject it. No live `0xdd03` session has been analysed to settle which behaviour is right.
2. **InfoV1 vs InfoV2.** Byte-identical layouts, different magics. No behavioural difference identified.
3. **`fps` in Info frames.** On older cameras this is documented as an index into a lookup table rather than a frame rate. The table is unknown.
4. **I-frame `unknown` fields.** Observed values `00 / 23 / 5A` at offset 20 and `00 / 06 / 29 / C3` in the additional-header remainder. Possibly NVR channel accounting.
5. **`Extension.userName` on ability queries.** Why the camera cannot infer the identity from the authenticated session. Potentially an authorisation bypass; untested.
6. **`Extension.checkPos` / `checkValue`.** Described as a decryption self-check; the verification algorithm is unconfirmed.
7. **`UdpAck.maybe_latency`.** Changes roughly once per second. Plausibly an RTT estimate.
8. **ADPCM `half_block_size`.** Literally `2` on some cameras, half the block size on others. Not needed to decode, but the discrepancy is unexplained.
9. **`MSG_ID_SET_DST` (107).** Symmetric to 106 by inference; never observed on the wire.
10. **`0x0fedcba0` reversed magic.** The endianness-hint theory fits the observations but has not been falsified — no known payload requires acting on it.
11. **`D2M_Q` `<r>` field.** Values `2` and `3` observed. A firmware revision, on the evidence of nothing but position.
12. **Mid-session re-anchoring.** Never observed. Whether a camera can re-anchor without restarting from `D2M_Q` is unknown.

---

## 14. Provenance

### Independent implementations

Listed alphabetically. Each was read as a separate witness to the wire format; agreement between two written in different languages is treated as stronger evidence than either alone.

- [borexola/neolink.net](https://github.com/borexola/neolink.net) — C#/.NET. Header codec and the BCEncrypt/AES/FullAes ladder.
- [thirtythreeforty/neolink](https://github.com/thirtythreeforty/neolink) — Rust. The origin of most public BC knowledge, and the ancestor of several later implementations, so it is not fully independent of them.
- [neolink `dissector/baichuan.lua`](https://github.com/thirtythreeforty/neolink/blob/master/dissector/baichuan.lua) — Wireshark dissector; deobfuscates XML in command messages. Its `protocol.md` and `messages.md` are the only prose specification predating this one, and are the source for the ⚑-marked message IDs in §5 and the alternate byte 12–15 reading in §2.1.
- [starkillerOG/reolink_aio](https://github.com/starkillerOG/reolink_aio) — Python. Independently confirms `DEFAULT_BC_PORT = 9000`, `HEADER_MAGIC = "f0debc0a"`, the `XML_KEY` bytes, `AES_IV = "0123456789abcdef"`, and the UDP key words.
- [verheesj/reolink-aio-ts](https://github.com/verheesj/reolink-aio-ts) — TypeScript.

Background: [Hacking Reolink cameras for fun and profit](https://www.thirtythreeforty.net/posts/hacking-reolink-cameras/) covers the protocol's discovery and the "port 9000" naming.

### What is documented here for the first time

The following are not covered by any source above and rest on captures against live hardware plus reverse-engineering of the official Reolink client: the **sigV3 / ECDHE login** (§4.4–4.5, incl. the PBKDF2 derivation and the post-login FullAes switch), the camera-local **`authLogin` / `getAccesskey`** exchange (§4.3), the **camera-side P2P verbs** `D2M_Q` / `M2D_Q_R` / `D2R_R` / `R2D_R_R` / `D2R_HB` (§9), and the **battery-camera boot and wake sequences** (§9.6–9.7). These carry correspondingly more `[I]` marks.

### Method and limits

Packet capture against live hardware; byte-exact replay of captured frames through independent parsers; cross-reading of the implementations above. The sigV3 and `authLogin` derivations come from reverse-engineering the official Reolink application (`BaichuanDevice::signatureLoginV3`, `BaichuanDevice.cpp` `authCodeLogin`). No Reolink documentation, source, or specification was consulted, because none is published.

Known limits on coverage, which bound how far the `[V]` marks generalise:

- Hardware for the battery-specific paths was **Argus-class on firmware `v3.0.x`**. Model-dependent behaviour is called out where known (§6.3 stream handles differ on E1, Swann and B800), but the survey is not broad.
- **NVRs and doorbells were not tested.** `channel_id` is specified as the NVR channel selector on the strength of field naming and single-channel captures only.
- The message catalogue (§5) is skewed toward what battery-camera clients exercise. §5.6 lists IDs attested only by the dissector, and their payload shapes are unverified.
- Nothing here was tested against firmware newer than the sigV3 rollout, which is the most actively changing part of the protocol.
