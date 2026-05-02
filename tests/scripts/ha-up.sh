#!/usr/bin/env bash
# Bring up the Home Assistant test rig (HA container + mosquitto).
#
# On macOS, also manages a colima VM as the Docker host. On Linux,
# Docker is assumed to run natively and the colima steps are skipped.
#
# Idempotent. On macOS, tracks whether THIS script started colima via
# the marker file tests/logs/colima.started-by-us — ha-down.sh /
# ha-verify.sh consume the marker to decide whether to stop colima on
# teardown, so a pre-existing colima used for other purposes is left
# alone. On Linux there is no marker file (no colima to track).
#
# Usage:
#     tests/scripts/ha-up.sh                 # start colima (macOS) + HA container
#     tests/scripts/ha-up.sh --config-only   # skip colima; just start/restart HA
#     tests/scripts/ha-up.sh --wait 60       # override API-ready timeout (default 30s)
#
# Prerequisites (one-time, not handled here — see docs/testing.md):
#   * Linux: docker (or podman with docker shim) reachable as the
#     calling user; mosquitto-clients for ha-verify.sh.
#   * macOS: colima + docker CLI installed (brew install colima docker
#     mosquitto).
#   * HA container created once with the bind mount documented in the
#     testing doc.
#   * tests/scripts/ha-config/configuration.yaml configured for bairelay
#     (in-repo so the rig travels; gitignored — HA persists tokens and
#     secrets here).
#
# Exit codes:
#     0  HA is up and responding on http://localhost:8123
#     1  startup failed

set -u
set -o pipefail

CONTAINER_NAME="${HA_CONTAINER:-homeassistant}"
MQTT_CONTAINER="${MQTT_CONTAINER:-bairelay-mosquitto}"
MQTT_IMAGE="${MQTT_IMAGE:-eclipse-mosquitto:2}"
HA_URL="${HA_URL:-http://localhost:8123}"

# Resolve repo root so this script works regardless of CWD.
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
LOG_DIR="$REPO_ROOT/tests/logs"
COLIMA_MARKER="$LOG_DIR/colima.started-by-us"
HA_CONFIG_DIR="${HA_CONFIG_DIR:-$REPO_ROOT/tests/scripts/ha-config}"
MQTT_CONFIG_DIR="${MQTT_CONFIG_DIR:-$REPO_ROOT/tests/scripts/mosquitto}"
MQTT_PORT="${MQTT_TEST_PORT:-1884}"

WAIT_SECONDS=30
CONFIG_ONLY=0

# OS detection: colima is macOS-only; on Linux Docker runs natively.
case "$(uname -s)" in
	Darwin) IS_MACOS=1 ;;
	Linux)  IS_MACOS=0 ;;
	*)      echo "unsupported OS: $(uname -s) (Linux or macOS required)" >&2; exit 1 ;;
esac

while [ $# -gt 0 ]; do
	case "$1" in
		--config-only) CONFIG_ONLY=1; shift ;;
		--wait) WAIT_SECONDS="$2"; shift 2 ;;
		--help|-h)
			sed -n '3,31p' "$0"
			exit 0 ;;
		*) echo "unknown arg: $1" >&2; exit 2 ;;
	esac
done

mkdir -p "$LOG_DIR"

log() { printf '[ha-up] %s\n' "$*"; }

# Step 1: colima (macOS only — on Linux Docker runs natively).
if [ "$CONFIG_ONLY" = 0 ] && [ "$IS_MACOS" = 1 ]; then
	if ! command -v colima >/dev/null 2>&1; then
		echo "colima not installed (brew install colima)" >&2
		exit 1
	fi
	if colima status >/dev/null 2>&1; then
		log "colima already running (leaving alone)"
	else
		log "starting colima..."
		if colima start; then
			# Record that WE started it so ha-down / ha-verify can undo later.
			date -u +%FT%TZ > "$COLIMA_MARKER"
			log "colima started by us (marker: $COLIMA_MARKER)"
		else
			echo "colima start failed" >&2
			exit 1
		fi
	fi
fi

# Step 2: docker reachable.
if ! docker info >/dev/null 2>&1; then
	if [ "$IS_MACOS" = 1 ]; then
		echo "docker not reachable; is colima up?" >&2
	else
		echo "docker not reachable; is the daemon running and is your user in the docker group?" >&2
	fi
	exit 1
fi

# Step 2.5: pin `host.docker.internal` IPv4-only in the colima VM
# `/etc/hosts` (macOS only). With `--network=host`, HA + go2rtc
# inherit this file at container start and resolve
# `host.docker.internal` via files (no DNS, no AAAA query). Without
# this entry, `getaddrinfo()` triggers an AAAA lookup that the colima
# resolver can return EAI_AGAIN for, breaking every HA `camera_proxy`
# probe with `Failed to resolve hostname host.docker.internal: Try
# again`. Idempotent — runs only when the entry is absent.
#
# On Linux there is no VM in front of Docker; the
# `--add-host=host.docker.internal:host-gateway` flag passed at
# container creation handles this directly inside the container.
if [ "$IS_MACOS" = 1 ]; then
	if ! colima ssh -- grep -q '\bhost\.docker\.internal\b' /etc/hosts 2>/dev/null; then
		log "pinning host.docker.internal -> 192.168.5.2 in colima /etc/hosts (IPv4-only)..."
		colima ssh -- sudo sh -c 'echo "192.168.5.2 host.docker.internal" >> /etc/hosts'
	fi
fi

# Step 3: HA container.
if ! docker inspect "$CONTAINER_NAME" >/dev/null 2>&1; then
	if [ "$IS_MACOS" = 1 ]; then
		ADD_HOST_FLAG='--add-host host.docker.internal:192.168.5.2'
		ADD_HOST_NOTE='The --add-host flag pins host.docker.internal IPv4-only inside the
container; without it the colima resolver returns EAI_AGAIN on AAAA
queries and ffmpeg fails with "Failed to resolve hostname
host.docker.internal: Try again" on every camera_proxy probe.
ha-up.sh also pins this in the colima VM /etc/hosts as a belt-and-
braces backup for cases where the container was created without
--add-host.'
	else
		ADD_HOST_FLAG='--add-host host.docker.internal:host-gateway'
		ADD_HOST_NOTE='The --add-host flag uses Docker'\''s host-gateway keyword so
host.docker.internal resolves to the host inside the container.
Required because Linux Docker does not provide host.docker.internal
out of the box, unlike Docker Desktop / colima.'
	fi
	cat >&2 <<EOF
container '$CONTAINER_NAME' does not exist. Create it once with:

  mkdir -p "$HA_CONFIG_DIR"
  docker run -d --name $CONTAINER_NAME \\
      --restart=unless-stopped \\
      -v "$HA_CONFIG_DIR:/config" \\
      --network=host \\
      $ADD_HOST_FLAG \\
      ghcr.io/home-assistant/home-assistant:stable

$ADD_HOST_NOTE

Then rerun tests/scripts/ha-up.sh.
EOF
	exit 1
fi

STATE=$(docker inspect -f '{{.State.Status}}' "$CONTAINER_NAME")
case "$STATE" in
	running)
		log "container '$CONTAINER_NAME' already running"
		;;
	exited|created|paused)
		log "starting container '$CONTAINER_NAME' (was $STATE)..."
		docker start "$CONTAINER_NAME" >/dev/null
		;;
	*)
		echo "container in unexpected state: $STATE" >&2
		exit 1 ;;
esac

# Step 3b: test mosquitto container on the same Docker host as HA.
# Both use --network=host so they share the host's network namespace
# (Linux: host directly; macOS: colima VM via port-forward shim); HA
# reaches mosquitto at 127.0.0.1:$MQTT_PORT and bairelay reaches it at
# the same 127.0.0.1:$MQTT_PORT. Stateless: persistence is off in
# mosquitto.conf, so the container can be recreated freely.
if ! docker image inspect "$MQTT_IMAGE" >/dev/null 2>&1; then
	cat >&2 <<EOF
mosquitto image '$MQTT_IMAGE' missing. Pull it once with:

  docker pull $MQTT_IMAGE

Then rerun tests/scripts/ha-up.sh.
EOF
	exit 1
fi
if docker inspect "$MQTT_CONTAINER" >/dev/null 2>&1; then
	MSTATE=$(docker inspect -f '{{.State.Status}}' "$MQTT_CONTAINER")
	case "$MSTATE" in
		running)
			log "container '$MQTT_CONTAINER' already running"
			;;
		exited|created|paused)
			log "starting container '$MQTT_CONTAINER' (was $MSTATE)..."
			docker start "$MQTT_CONTAINER" >/dev/null
			;;
		*)
			echo "mosquitto container in unexpected state: $MSTATE" >&2
			exit 1 ;;
	esac
else
	log "creating container '$MQTT_CONTAINER' from $MQTT_IMAGE..."
	if ! docker run -d --name "$MQTT_CONTAINER" \
			--restart=unless-stopped \
			--network=host \
			-v "$MQTT_CONFIG_DIR/mosquitto.conf:/mosquitto/config/mosquitto.conf:ro" \
			-v "$MQTT_CONFIG_DIR/passwd:/mosquitto/config/passwd:ro" \
			"$MQTT_IMAGE" >/dev/null 2>&1; then
		echo "failed to start mosquitto container" >&2
		docker logs --tail=30 "$MQTT_CONTAINER" >&2 2>/dev/null || true
		exit 1
	fi
fi
# Confirm it's actually listening before we hand off to HA.
for _ in 1 2 3 4 5; do
	if nc -z 127.0.0.1 "$MQTT_PORT" 2>/dev/null; then
		break
	fi
	sleep 1
done
if ! nc -z 127.0.0.1 "$MQTT_PORT" 2>/dev/null; then
	echo "mosquitto did not bind :$MQTT_PORT" >&2
	docker logs --tail=30 "$MQTT_CONTAINER" >&2 2>/dev/null || true
	exit 1
fi
log "mosquitto listening on :$MQTT_PORT"

# Step 4: wait for API.
log "waiting up to ${WAIT_SECONDS}s for HA API at $HA_URL ..."
HA_READY=0
for i in $(seq 1 "$WAIT_SECONDS"); do
	code=$(curl -s -o /dev/null -w '%{http_code}' "$HA_URL/" || true)
	if [ "$code" = "200" ] || [ "$code" = "302" ]; then
		log "HA ready after ${i}s (HTTP $code)"
		HA_READY=1
		break
	fi
	sleep 1
done
if [ "$HA_READY" = 0 ]; then
	echo "HA did not respond on $HA_URL within ${WAIT_SECONDS}s" >&2
	echo "tail of container logs:" >&2
	docker logs --tail=20 "$CONTAINER_NAME" >&2
	exit 1
fi

# Step 5: ensure HA's MQTT integration is configured to point at our
# test broker. Idempotent — skip if any mqtt config entry already
# exists. Requires tests/ha-token.
TOKEN_FILE="${HA_TOKEN_FILE:-$REPO_ROOT/tests/ha-token}"
if [ -s "$TOKEN_FILE" ]; then
	TOKEN=$(tr -d '\r\n[:space:]' < "$TOKEN_FILE")
	EXISTING=$(curl -s -H "Authorization: Bearer $TOKEN" \
		"$HA_URL/api/config/config_entries/entry?domain=mqtt" \
		| python3 -c 'import json,sys; print(len(json.load(sys.stdin)))' 2>/dev/null || echo 0)
	if [ "$EXISTING" -gt 0 ] 2>/dev/null; then
		log "HA MQTT integration already configured (count=$EXISTING)"
	else
		log "configuring HA MQTT integration → host.docker.internal:$MQTT_PORT..."
		FLOW_RSP=$(curl -s -X POST -H "Authorization: Bearer $TOKEN" \
			-H "Content-Type: application/json" \
			-d '{"handler":"mqtt"}' \
			"$HA_URL/api/config/config_entries/flow")
		FLOW_ID=$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("flow_id",""))' <<< "$FLOW_RSP")
		if [ -z "$FLOW_ID" ]; then
			echo "could not start MQTT flow: $FLOW_RSP" >&2
			exit 1
		fi
		PAYLOAD=$(python3 -c 'import json,sys; print(json.dumps({"broker":"host.docker.internal","port":int(sys.argv[1]),"username":"bairelay-test","password":"bairelay-test-password"}))' "$MQTT_PORT")
		FINAL=$(curl -s -X POST -H "Authorization: Bearer $TOKEN" \
			-H "Content-Type: application/json" \
			-d "$PAYLOAD" \
			"$HA_URL/api/config/config_entries/flow/$FLOW_ID")
		if ! grep -q '"type": *"create_entry"' <<< "$FINAL"; then
			echo "MQTT flow did not create entry: $FINAL" >&2
			exit 1
		fi
		log "HA MQTT integration created"
	fi
else
	log "no HA token at $TOKEN_FILE; skipping MQTT integration setup"
fi

exit 0
