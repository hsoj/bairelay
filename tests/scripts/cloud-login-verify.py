#!/usr/bin/env python3
"""Independent confirmation oracle for bairelay's cloud ("account device") login.

This is a pure-Python reimplementation of the Reolink app's cloud sigV3 login,
written straight from the reverse-engineered protocol. It does NOT use bairelay
— it talks to Reolink's cloud and to the camera directly — so it can confirm,
independently of the Rust code, that:

  1. a Reolink account can mint a device access-authorization bundle, and
  2. the camera accepts the sigV3 login built from that bundle (response 200).

If this script gets a 200 but bairelay does not (or vice versa), the divergence
is in bairelay's implementation, not the protocol. That is what makes it useful.

WHAT IT DOES
  Step 1  OAuth: password grant -> refresh grant (grant_session_code=true),
          yielding a short-lived bearer token.
  Step 2  POST /v2/devices/access-authorization -> {token{p,s,k}, certChain},
          solving a hashcash proof-of-work if the cloud challenges (code 8214).
  Step 3  (--login) Pure-Python sigV3 login over UDP: broadcast the lver=3
          C2D_C connect, read the camera's D2C_C_R (login nonce + ECDHE offer),
          build the AES-CFB cipherContent + LoginUser, frame it as
          Baichuan-over-BcUdp, and read back the camera's response_code.

  Two load-bearing wire details are baked in:
    * the Bc message class is 0x0000 (NOT the generic modern-login 0x6414), and
    * certChain newlines are emitted as &#x0A; entities so the camera's
      whitespace-condensing TiXml parser preserves the PEM.

HOW TO USE
  Credentials come from the environment — never hardcoded, never printed:
      REOLINK_EMAIL      Reolink account email
      REOLINK_PASSWORD   Reolink account password
      REOLINK_UID        target camera UID (16 chars)
      CAMERA_IP          (optional) camera LAN IP for unicast discovery; without
                         it the script only broadcasts on the local subnet
      CAMERA_USER        (optional) camera login user, default "admin"
  Keep them out of your shell history — e.g. source a private env file you
  hold outside the repo (`set -a; . ~/.reolink.env; set +a`) before running.

  Confirm the cloud bundle only:
      REOLINK_EMAIL=… REOLINK_PASSWORD=… REOLINK_UID=… \
          python3 tests/scripts/cloud-login-verify.py

  Full end-to-end (camera must be awake and on the same L2 network):
      CAMERA_IP=192.0.2.10 … python3 tests/scripts/cloud-login-verify.py --login

  Probe the login-MFA / 30-day trust-token flow (run from the UNTRUSTED host
  that gets the 8208 "mfa_required" — it triggers a real email and prompts for
  the code, then proves whether a stored trust token clears MFA headlessly):
      REOLINK_EMAIL=… REOLINK_PASSWORD=… python3 tests/scripts/cloud-login-verify.py --mfa

REQUIREMENTS
  Python 3.9+ and the `cryptography` package (X25519 + AES-CFB):
      pip install cryptography
"""
import base64
import hashlib
import json
import os
import re
import socket
import struct
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

from cryptography.hazmat.primitives.asymmetric.x25519 import (
    X25519PrivateKey, X25519PublicKey)
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

BASE = "https://apis.reolink.com"
# The official iOS app's public client identifier + User-Agent. These are the
# same for every user of the app (they identify the app, not the account) and
# are required for the cloud API to answer.
CLIENT_ID = "REO-BHAPEi1tILWrc37S|Zit"
UA = "Reolink iOS App/4.60.3.0 (REO-BHAPEi1tILWrc37S|Zit; iPadOS/26.4)"

EMAIL = os.environ.get("REOLINK_EMAIL")
PASSWORD = os.environ.get("REOLINK_PASSWORD")
UID = os.environ.get("REOLINK_UID")
CAMERA_IP = os.environ.get("CAMERA_IP")
CAMERA_USER = os.environ.get("CAMERA_USER", "admin")
# The --mfa probe is account-only (no camera), so it doesn't need a UID.
_need_uid = "--mfa" not in sys.argv
if not EMAIL or not PASSWORD or (_need_uid and not UID):
    need = "REOLINK_EMAIL / REOLINK_PASSWORD" + ("" if not _need_uid else " / REOLINK_UID")
    sys.exit(f"error: set {need} in the environment")


def redact(t):
    """Show only a value's length + last 4 chars — never the secret itself."""
    s = str(t or "")
    return f"<len={len(s)} …{s[-4:]}>" if s else "<none>"


# ---------------------------------------------------------------------------
# Cloud HTTP
# ---------------------------------------------------------------------------
def _headers(bearer=None, ct=None):
    h = {
        "accept": "*/*",
        "x-api-challenge-accept": "pow/1,captcha/1",
        "x-client-id": CLIENT_ID,
        "user-agent": UA,
        "accept-encoding": "identity",
    }
    if ct:
        h["content-type"] = ct
    if bearer:
        h["authorization"] = "Bearer " + bearer
    return h


def _send(method, url, *, body=None, ct=None, bearer=None, challenge=None, extra_headers=None):
    h = _headers(bearer, ct)
    if challenge:
        h["X-Api-Challenge"] = challenge
    if extra_headers:
        h.update(extra_headers)
    data = body.encode() if isinstance(body, str) else body
    req = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def _jbody(raw):
    try:
        return json.loads(raw.decode())
    except Exception:
        return None


# ---- hashcash proof-of-work (only triggered on error.code 8214) ----
def _lzbits(b):
    n = 0
    for byte in b:
        if byte == 0:
            n += 8
            continue
        for i in range(7, -1, -1):
            if (byte >> i) & 1:
                return n
            n += 1
    return n


def _solve_pow(ch):
    cid = ch["id"]
    d = ch["data"]
    prefix = d["r"].encode()
    charset = d["c"]
    nonce = None
    for p in d["p"]:
        diff = p["d"]
        i = 0
        while True:
            cand = "".join(charset[(i >> (5 * k)) % len(charset)] for k in range(32))
            if _lzbits(hashlib.sha256(prefix + cand.encode()).digest()) >= diff:
                nonce = cand
                break
            i += 1
    return f"type=pow/1;id={cid};token={nonce}"


def _post_json(path, obj, bearer, label):
    body = json.dumps(obj)
    st, raw = _send("POST", BASE + path, body=body, ct="application/json", bearer=bearer)
    j = _jbody(raw)
    err = j.get("error") if isinstance(j, dict) else None
    if err and err.get("code") == 8214:
        ch = err.get("metadata", {}).get("challenge")
        st, raw = _send("POST", BASE + path, body=body, ct="application/json",
                        bearer=bearer, challenge=_solve_pow(ch))
        j = _jbody(raw)
    print(f"  [{label}] {st}")
    return st, j, raw


def authenticate():
    """password grant -> refresh grant w/ session code -> bearer token."""
    st, raw = _send("POST", BASE + "/v1.0/oauth2/token/",
                    body=urllib.parse.urlencode({
                        "client_id": CLIENT_ID, "grant_type": "password",
                        "username": EMAIL, "password": PASSWORD,
                    }), ct="application/x-www-form-urlencoded")
    j = _jbody(raw) or {}
    refresh, tok = j.get("refresh_token"), j.get("access_token")
    print(f"  [password grant] {st} access={redact(tok)} refresh={redact(refresh)}")
    if not refresh:
        print("  body:", raw[:300])
        return None
    st, raw = _send("POST", BASE + "/v1.0/oauth2/token/",
                    body=urllib.parse.urlencode({
                        "refresh_token": refresh, "client_id": CLIENT_ID,
                        "grant_session_code": "true", "grant_type": "refresh_token",
                    }), ct="application/x-www-form-urlencoded")
    j = _jbody(raw) or {}
    if j.get("access_token"):
        tok = j["access_token"]
    print(f"  [refresh+session_code] {st} access={redact(tok)} "
          f"session_code={'yes' if j.get('web_session_auth_code') else 'no'}")
    return tok


# ===========================================================================
# MFA PROBE — does the email-MFA + mfa_trust_token flow let a headless host
# clear Reolink's login verification and STAY cleared (~30 days)?  Sequence
# RE'd from the Reolink RN bundle: codes -> sessions -> token (session_mode),
# with mfa_trusted / mfa_trust_token carried in the grant body. Run this ONCE
# from the untrusted server; it triggers a real email and uses one MFA attempt.
# ===========================================================================
def _dig(j, key):
    """Pull `key` from a JSON body that may be top-level or nested under `data`."""
    if not isinstance(j, dict):
        return None
    if key in j:
        return j[key]
    d = j.get("data")
    return d.get(key) if isinstance(d, dict) else None


def _redacted_json(j):
    """Render a token response with secret values blanked but all keys visible."""
    secret = {"access_token", "refresh_token", "id_token",
              "web_session_auth_code", "mfa_trust_token", "trust_token"}

    def scrub(o):
        if isinstance(o, dict):
            return {k: (redact(v) if k in secret else scrub(v)) for k, v in o.items()}
        if isinstance(o, list):
            return [scrub(x) for x in o]
        return o

    return json.dumps(scrub(j), indent=2)


def _grant(extra, *, verify_id=None, verify_code=None):
    """The app's MFA login grant: password grant with session_mode=true, the
    x-verify-scenario header, and (when verifying) the x-verify-id/code headers."""
    headers = {"x-verify-scenario": "users.login_with_password"}
    if verify_id is not None:
        headers["x-verify-id"] = verify_id
    if verify_code is not None:
        headers["x-verify-code"] = verify_code
    form = {
        "username": EMAIL, "password": PASSWORD, "grant_type": "password",
        "session_mode": "true", "client_id": CLIENT_ID,
    }
    form.update(extra)
    st, raw = _send("POST", BASE + "/v1.0/oauth2/token/",
                    body=urllib.parse.urlencode(form),
                    ct="application/x-www-form-urlencoded", extra_headers=headers)
    return st, _jbody(raw) or {}, raw


def mfa_probe():
    print("== 1. password grant (expect mfa_required from an untrusted IP) ==")
    st, j, raw = _grant({})
    if "access_token" in j:
        print("  grant SUCCEEDED with no MFA — this IP is already trusted; run "
              "it from the untrusted server to exercise the MFA path.")
        return 0
    err = j.get("error", {}) if isinstance(j, dict) else {}
    if err.get("symbol") != "mfa_required" and err.get("code") != 8208:
        print(f"  unexpected {st}: {raw[:300]}")
        return 1
    methods = list((err.get("metadata", {}).get("allowMethods") or {}).keys())
    print(f"  mfa_required as expected; allowMethods={methods}")

    print("== 2. trigger the email code (POST /v2/auth/mfa/codes, method=email) ==")
    st, raw = _send("POST", BASE + "/v2/auth/mfa/codes",
                    body=json.dumps({"clientId": CLIENT_ID,
                                     "scenario": "users.login_with_password",
                                     "method": "email",
                                     "data": {"emailAddress": EMAIL}}),
                    ct="application/json")
    code_id = _dig(_jbody(raw) or {}, "id")
    print(f"  [mfa/codes] {st} id={code_id!r}")
    if not code_id:
        print("  no code id:", raw[:300])
        return 1

    code = input(f"  >> paste the code Reolink emailed to {EMAIL}: ").strip()

    print("== 3. exchange code for an MFA session (POST /v2/auth/mfa/sessions) ==")
    st, raw = _send("POST", BASE + "/v2/auth/mfa/sessions",
                    body=json.dumps({"id": code_id, "code": code}),
                    ct="application/json")
    sess = _jbody(raw) or {}
    sess_id, sess_code = _dig(sess, "id"), _dig(sess, "code")
    print(f"  [mfa/sessions] {st} id={sess_id!r} code={redact(sess_code)}")
    if not sess_id or not sess_code:
        print("  no MFA session returned:", raw[:300])
        return 1

    print("== 4. verified login (session_mode + x-verify headers, mfa_trusted) ==")
    st, j, raw = _grant({"mfa_trusted": "true"},
                        verify_id=str(sess_id), verify_code=str(sess_code))
    if "access_token" not in j:
        print(f"  verified login FAILED {st}: {raw[:400]}")
        return 1
    print(f"  verified login OK ({st}). Full response (secrets redacted):")
    print(_redacted_json(j))

    trust = next((_dig(j, k) for k in
                  ("mfa_trust_token", "trust_token", "mfaTrustToken") if _dig(j, k)), None)
    if not trust:
        print("  NOTE: no obvious trust-token field above. Inspect the keys — the "
              "30-day token may be named differently (or set via a cookie). Phase 5 "
              "needs it to prove headless reuse.")
        return 0
    print(f"  trust token issued: {redact(trust)}")

    print("== 5. PROOF: fresh grant WITH the trust token, NO email/verify ==")
    st, j, raw = _grant({"mfa_trusted": "true", "mfa_trust_token": trust})
    if "access_token" in j:
        print("  *** SUCCESS: the trust token cleared MFA with no email. A one-"
              "time bootstrap + a stored trust token = ~30 days hands-off. ***")
        return 0
    print(f"  *** trust token did NOT skip MFA {st}: {raw[:400]} ***")
    print("  (headless reuse NOT confirmed — each run would re-prompt for email.)")
    return 1


def mint_bundle(bearer):
    """POST /v2/devices/access-authorization -> {token{p,s,k}, certChain}."""
    st, j, raw = _post_json("/v2/devices/access-authorization",
                            {"uid": UID, "protocol": 3, "certChain": True},
                            bearer, "access-authorization")
    data = j.get("data") if isinstance(j, dict) and isinstance(j.get("data"), dict) else j
    if not isinstance(data, dict) or "token" not in data:
        print("  bundle FAILED:", raw[:300])
        return None
    pj = json.loads(data["token"]["p"])
    print(f"  bundle OK: sid={pj.get('sid')} sub={pj.get('sub')} "
          f"role={pj.get('role')} iat={pj.get('iat')} exp={pj.get('exp')}")
    return data


# ===========================================================================
# LOCAL sigV3 LOGIN — pure-Python replica of the camera's P2P + Baichuan login
# ===========================================================================
BC_MAGIC = 0x0ABCDEF0
UDP_NEGO, UDP_ACK, UDP_DATA = 0x2A87CF3A, 0x2A87CF20, 0x2A87CF10
# BcUdp discovery XOR keystream words (per-tid offset).
DISC_KEY = [0x1F2D3C4B, 0x5A6C7F8D, 0x38172E4B, 0x8271635A,
            0x863F1A2B, 0xA5C6F7D8, 0x8371E1B4, 0x17F2D3A5]
# Bc body BCEncrypt key (XOR, offset = channel_id = 0).
BC_KEY = bytes([0x1F, 0x2D, 0x3C, 0x4B, 0x5A, 0x69, 0x78, 0xFF])
MTU_CHUNK = 1330
# Reolink discovery ports the camera listens on.
DISC_PORTS = (2018, 2015)


def _calc_crc(data):
    """Reflected CRC-32 (poly 0x04C11DB7, init 0, xorout 0) — the Bc variant."""
    crc = 0
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0xEDB88320 if crc & 1 else 0)
    return crc & 0xFFFFFFFF


def _disc_xor(tid, buf):
    ks = b"".join(struct.pack("<I", (w + tid) & 0xFFFFFFFF) for w in DISC_KEY)
    ks = (ks * (len(buf) // len(ks) + 1))[:len(buf)]
    return bytes(a ^ b for a, b in zip(buf, ks))


def _bc_xor(buf, offset=0):
    return bytes(b ^ BC_KEY[(i + offset) % 8] ^ (offset & 0xFF)
                 for i, b in enumerate(buf))


def _md5_31(s):
    return hashlib.md5(s.encode()).hexdigest().upper()[:31]


def _jesc(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')


def _disc_packet(tid, xml):
    enc = _disc_xor(tid, xml.encode())
    return struct.pack("<IIIII", UDP_NEGO, len(enc), 1, tid, _calc_crc(enc)) + enc


def _data_packet(conn_id, pkt_id, chunk):
    return struct.pack("<IiIII", UDP_DATA, conn_id, 0, pkt_id, len(chunk)) + chunk


def _ack_packet(conn_id, pkt_id):
    return struct.pack("<IiIIIII", UDP_ACK, conn_id, 0, 0, pkt_id, 0, 0)


def _parse_pl(pl):
    """Camera ECDHE pubkey (P4) + iterations (P6) from the D2C_C_R <pl> line."""
    p4 = p6 = None
    for tok in pl.replace(";", ",").split(","):
        tok = tok.strip()
        if tok.startswith("P4="):
            p4 = tok[3:]
        elif tok.startswith("P6="):
            p6 = int(tok[3:])
    return p4, p6


def _build_login_xml(nc, cam_pub_b64, iters, bundle):
    nonce = str(nc)
    user_name = _md5_31(CAMERA_USER + nonce)
    password = _md5_31(nonce)  # account camera: md5("" + nonce)

    priv = X25519PrivateKey.generate()
    our_pub = priv.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)
    cam_pub = base64.b64decode(cam_pub_b64)
    shared = priv.exchange(X25519PublicKey.from_public_bytes(cam_pub))

    # PBKDF2 "password" is the NONCE zero-padded into a fixed 32-byte buffer
    # (first 31 bytes), salt is the raw X25519 shared secret.
    kdf = bytearray(32)
    nb = nonce.encode()
    kdf[:min(len(nb), 31)] = nb[:31]
    derived = hashlib.pbkdf2_hmac("sha256", bytes(kdf), shared, iters, 32)
    key, iv = derived[:16], derived[16:32]

    tok = bundle["token"]
    plain = ('{"nonce":"%s","clientTime":%d,"token":{"p":"%s","s":"%s"}}'
             % (nonce, int(time.time()), _jesc(tok["p"]), _jesc(tok["s"])))
    enc = Cipher(algorithms.AES(key), modes.CFB(iv)).encryptor()
    cipher_content = base64.b64encode(enc.update(plain.encode()) + enc.finalize()).decode()

    pub_b64 = base64.b64encode(our_pub).decode()
    # certChain newlines as &#x0A; entities (survives the camera's TiXml
    # whitespace condensing — a literal \n would be collapsed and the PEM lost).
    cert = bundle.get("certChain", "").replace("\r\n", "\n").replace("\n", "&#x0A;")
    xml = (
        '<?xml version="1.0" encoding="UTF-8" ?>\n<body>\n'
        '<LoginUser version="1.1">\n'
        f'<userName>{user_name}</userName>\n'
        f'<password>{password}</password>\n'
        '<userVer>1</userVer>\n'
        '<clientType>app</clientType>\n'
        f'<publicKey>{pub_b64}</publicKey>\n'
        f'<tokenKey>{tok["k"]}</tokenKey>\n'
        f'<cipherContent>{cipher_content}</cipherContent>\n'
        f'<certChain>{cert}</certChain>\n'
        '</LoginUser>\n'
        '<LoginNet version="1.1">\n<type>LAN</type>\n<udpPort>0</udpPort>\n</LoginNet>\n'
        '</body>\n')
    return xml, user_name


def _build_bc_login(xml):
    # sigV3 login uses Bc class 0x0000 + msg_num 0 (verified on the wire) —
    # NOT 0x6414, which the camera routes to a token-rejecting handler.
    body = _bc_xor(xml.encode(), 0)  # BCEncrypt, channel 0
    header = struct.pack("<IIIBBHHHI",
                         BC_MAGIC, 1, len(body), 0, 0, 0, 0, 0x0000, 0)
    return header + body


def _discovery_targets():
    targets = [("255.255.255.255", p) for p in DISC_PORTS]
    if CAMERA_IP:
        targets += [(CAMERA_IP, p) for p in DISC_PORTS]
    return targets


def local_sigv3_login(bundle, timeout=10.0):
    """Broadcast the lver=3 connect, send the sigV3 login, return response_code."""
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    sock.bind(("", 0))
    myport = sock.getsockname()[1]
    cid = 0x0000C0DE
    tid = 0x0003E7AA
    c2dc = (
        '<?xml version="1.0" encoding="UTF-8" ?>\n<P2P>\n<C2D_C>\n'
        f'<uid>{UID}</uid>\n<cli><port>{myport}</port></cli>\n<cid>{cid}</cid>\n'
        '<mtu>1350</mtu>\n<debug>0</debug>\n<p>MAC</p>\n<lver>3</lver>\n</C2D_C>\n</P2P>\n')
    pkt = _disc_packet(tid, c2dc)
    targets = _discovery_targets()

    # --- discovery: broadcast (+ optional unicast) until D2C_C_R ---
    did = nc = pl = cam_addr = None
    sock.settimeout(1.0)
    deadline = time.time() + timeout
    while time.time() < deadline and did is None:
        for t in targets:
            try:
                sock.sendto(pkt, t)
            except OSError:
                pass
        try:
            data, addr = sock.recvfrom(8192)
        except socket.timeout:
            continue
        if len(data) < 20 or struct.unpack_from("<I", data, 0)[0] != UDP_NEGO:
            continue
        psz, _u, rtid = struct.unpack_from("<III", data, 4)
        xmlr = _disc_xor(rtid, data[20:20 + psz]).decode("utf-8", "replace")
        if "D2C_C_R" not in xmlr:
            continue
        did = int(re.search(r"<did>(-?\d+)</did>", xmlr).group(1))
        ncm = re.search(r"<nc>(-?\d+)</nc>", xmlr)
        plm = re.search(r"<pl>(.*?)</pl>", xmlr, re.S)
        nc = int(ncm.group(1)) if ncm else None
        pl = plm.group(1) if plm else None
        cam_addr = addr
        print(f"  D2C_C_R from {addr}: did={did} nc={nc}")
    if did is None:
        print("  DISCOVERY TIMEOUT — camera asleep / unreachable")
        return None
    if nc is None or not pl:
        print("  camera did NOT offer sigV3 (no nc/pl) — not an account camera?")
        return None

    cam_pub_b64, iters = _parse_pl(pl)
    xml, un = _build_login_xml(nc, cam_pub_b64, iters, bundle)
    print(f"  login: nonce={nc} iters={iters} userName={un}")
    bc = _build_bc_login(xml)
    chunks = [bc[i:i + MTU_CHUNK] for i in range(0, len(bc), MTU_CHUNK)]
    for pid, ch in enumerate(chunks):
        sock.sendto(_data_packet(did, pid, ch), cam_addr)
    print(f"  sent login: {len(bc)} bytes in {len(chunks)} packet(s) to {cam_addr}")

    # --- read response: first camera DATA carries the Bc header ---
    sock.settimeout(1.0)
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            data, addr = sock.recvfrom(8192)
        except socket.timeout:
            continue
        if len(data) < 8:
            continue
        magic = struct.unpack_from("<I", data, 0)[0]
        if magic == UDP_DATA:
            _conn_id, _z, pid, psz = struct.unpack_from("<iIII", data, 4)
            payload = data[20:20 + psz]
            sock.sendto(_ack_packet(did, pid), cam_addr)  # ack the camera
            if len(payload) >= 18 and struct.unpack_from("<I", payload, 0)[0] == BC_MAGIC:
                resp = struct.unpack_from("<H", payload, 16)[0]
                msg_id = struct.unpack_from("<I", payload, 4)[0]
                print(f"  camera Bc reply: msg_id={msg_id} response_code={resp}")
                return resp
    print("  no Bc reply within timeout")
    return None


# ---------------------------------------------------------------------------
def main():
    if "--mfa" in sys.argv:
        # Probe the login-MFA / trust-token flow (run from the untrusted host).
        return mfa_probe()

    print("== auth ==")
    tok = authenticate()
    if not tok:
        return 1

    print("== access-authorization ==")
    bundle = mint_bundle(tok)
    if not bundle:
        return 1

    if "--login" not in sys.argv:
        print("\nbundle minted OK. Re-run with --login (camera awake, same LAN) "
              "to drive the sigV3 login.")
        return 0

    print("== local sigV3 login (pure python) ==")
    code = local_sigv3_login(bundle)
    if code == 200:
        print("  *** LOGIN ACCEPTED (200) ***")
        return 0
    if code is not None:
        print(f"  *** LOGIN REJECTED response_code={code} ***")
    return 1


if __name__ == "__main__":
    sys.exit(main())
