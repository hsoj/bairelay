#!/usr/bin/env bash
# Generate self-signed test certs for manual-verify.sh --tls and any
# operator who wants to point bairelay at a real PEM. Idempotent —
# regenerates only when missing or expiring within 30 days.
#
# Output (all under tests/test-certs/, gitignored):
#   ca.pem            self-signed CA
#   ca.key            CA private key
#   server.pem        server leaf signed by CA
#   server.key        server private key
#   server-bundle.pem cat server.pem server.key (neolink-shape input)
#   client.pem        client leaf signed by CA
#   client.key        client private key
#
# Server cert SANs: localhost, 127.0.0.1, ::1, host.docker.internal.
# Validity: 10 years. Test-only material — do not deploy to production.

set -euo pipefail

cd "$(dirname "$0")/../.."
OUT=tests/test-certs
mkdir -p "$OUT"

needs_regen() {
	local f=$1
	[[ ! -f $f ]] && return 0
	# Re-gen if expiring within 30 days.
	if openssl x509 -in "$f" -checkend $((30*24*3600)) >/dev/null 2>&1; then
		return 1
	fi
	return 0
}

if needs_regen "$OUT/server.pem" || needs_regen "$OUT/ca.pem"; then
	echo "Generating CA..."
	openssl genrsa -out "$OUT/ca.key" 4096 2>/dev/null
	openssl req -x509 -new -nodes -key "$OUT/ca.key" \
		-sha256 -days 3650 \
		-subj "/CN=bairelay-test-ca" \
		-out "$OUT/ca.pem" 2>/dev/null

	echo "Generating server leaf..."
	openssl genrsa -out "$OUT/server.key" 4096 2>/dev/null

	cat > "$OUT/server.cnf" <<'EOF'
[req]
distinguished_name = req_distinguished_name
req_extensions = v3_req
prompt = no
[req_distinguished_name]
CN = localhost
[v3_req]
subjectAltName = @alt_names
[alt_names]
DNS.1 = localhost
DNS.2 = host.docker.internal
IP.1 = 127.0.0.1
IP.2 = ::1
EOF
	openssl req -new -key "$OUT/server.key" \
		-out "$OUT/server.csr" \
		-config "$OUT/server.cnf" 2>/dev/null
	openssl x509 -req -in "$OUT/server.csr" \
		-CA "$OUT/ca.pem" -CAkey "$OUT/ca.key" -CAcreateserial \
		-out "$OUT/server.pem" -days 3650 -sha256 \
		-extfile "$OUT/server.cnf" -extensions v3_req 2>/dev/null
	cat "$OUT/server.pem" "$OUT/server.key" > "$OUT/server-bundle.pem"

	echo "Generating client leaf..."
	openssl genrsa -out "$OUT/client.key" 4096 2>/dev/null
	openssl req -new -key "$OUT/client.key" \
		-subj "/CN=bairelay-test-client" \
		-out "$OUT/client.csr" 2>/dev/null
	openssl x509 -req -in "$OUT/client.csr" \
		-CA "$OUT/ca.pem" -CAkey "$OUT/ca.key" -CAcreateserial \
		-out "$OUT/client.pem" -days 3650 -sha256 2>/dev/null

	rm -f "$OUT/server.csr" "$OUT/client.csr" "$OUT/server.cnf" "$OUT/ca.srl"
	echo "Done. Test certs in $OUT/"
else
	echo "Test certs in $OUT/ are still valid (>30 days). Skipping regen."
fi
