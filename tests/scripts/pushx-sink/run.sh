#!/usr/bin/env bash
# Stand up an HTTPS sink on :443 pretending to be pushx.reolink.com so we
# can determine whether Argus firmware validates the cert. Runs `openssl
# s_server` with full handshake + payload tracing. Requires root to bind
# 443; takes Ctrl-C to stop.
#
# Use after DNS-hijacking pushx.reolink.com -> this host's LAN IP at the
# operator's resolver. Trigger motion in front of a camera; whatever
# arrives lands on stdout.

set -euo pipefail

cd "$(dirname "$0")"

[ -f cert.pem ] || { echo "cert.pem missing — regenerate via openssl req"; exit 1; }
[ -f key.pem ]  || { echo "key.pem missing — regenerate via openssl req";  exit 1; }

LOG="../../logs/real-pcap/pushx-sink-$(date -u +%Y%m%dT%H%M%SZ).log"
mkdir -p "$(dirname "$LOG")"

echo "logging to $LOG"
echo "press Ctrl-C to stop"

# -msg shows handshake messages as they're sent/received (cleartext side).
# -debug shows raw bytes. -www makes openssl reply with a benign HTTP
# page so the camera's POST gets a 2xx response and tears the
# connection cleanly instead of retrying. Without -www the response
# would just be the openssl prompt, which can confuse the client.
exec sudo openssl s_server \
	-accept 0.0.0.0:443 \
	-cert cert.pem \
	-key  key.pem \
	-www \
	-msg \
	-tls1_2 \
	2>&1 | tee "$LOG"
