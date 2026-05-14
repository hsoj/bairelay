#!/usr/bin/env bash
# End-to-end Home Assistant ingestion check for .
#
# Pipeline:
#   1. Ensure HA container is up (via tests/scripts/ha-up.sh; on macOS
#      this also covers the colima Docker host).
#   2. Build + start bairelay mqtt-rtsp on 0.0.0.0:8554.
#   3. Hold wake locks on each camera via MQTT control/wakeup.
#   4. Provision one "Generic Camera" config entry per bairelay stream
#      via HA's REST config-flow API. Idempotent: existing entries with
#      the same stream_source are reused. Entries are PERMANENT: this
#      script never deletes them, because the HA instance is dedicated
#      to bairelay testing and we want the cameras visible between runs
#      for manual inspection.
#   5. Resolve entry_id -> entity_id via the /api/template Jinja surface.
#   6. Pull a snapshot two ways per stream:
#        * HA /api/camera_proxy (Generic Camera + ffmpeg: wrapper).
#        * go2rtc native RTSP via the container's unix socket.
#      Assert each JPEG is >=1024 bytes and starts with ff d8 ff.
#      's root-cause fix (bairelay no longer emits in-band
#      VPS/SPS/PPS — SDP sprop-* is authoritative out-of-band carriage)
#      means /api/camera_proxy now works for HEVC main too.
#   7. Tear bairelay down. On macOS, if ha-up.sh (or this script, via
#      ha-up.sh) started colima, stop it and remove the marker file.
#      Linux has no colima to manage. Leave HA container + entries
#      untouched in either case.
#
# Usage:
#     tests/scripts/ha-verify.sh               # run against tests/bairelay-test.toml
#     tests/scripts/ha-verify.sh -c my.toml    # override config path
#     tests/scripts/ha-verify.sh --no-build    # skip cargo build
#     tests/scripts/ha-verify.sh --keep-colima # macOS only: leave colima
#                                              # running on exit even if
#                                              # we started it
#     tests/scripts/ha-verify.sh --bairelay-as-container  # run the HA add-on
#                                              # image instead of cargo-built
#                                              # bairelay; see docs/testing.md
#
# Prerequisites (one-time — see docs/testing.md):
#   * Linux: docker (or podman with docker shim) reachable as the
#     calling user; mosquitto-clients installed.
#   * macOS: colima + docker + mosquitto installed (brew install ...).
#   * HA container created with --network=host and
#     tests/scripts/ha-config/configuration.yaml (in-repo, gitignored)
#     configured for bairelay.
#   * tests/ha-token contains a long-lived HA access token.
#   * config.toml configured for the real cameras.
#
# Exit codes:
#     0  every probe passed (HA proxy + go2rtc native). As of #        HEVC main succeeds via /api/camera_proxy directly — no KNOWN
#        classifier.
#     1  one or more streams failed
#     2  setup error

set -u
set -o pipefail

CONFIG=""
NO_BUILD=0
KEEP_COLIMA=0
AS_CONTAINER=0
HA_URL="${HA_URL:-http://localhost:8123}"

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
OUT_DIR="$REPO_ROOT/tests/logs/ha-verify"
COLIMA_MARKER="$REPO_ROOT/tests/logs/colima.started-by-us"
TOKEN_FILE="${HA_TOKEN_FILE:-$REPO_ROOT/tests/ha-token}"
DEFAULT_TEST_CONFIG="$REPO_ROOT/tests/bairelay-test.toml"
ADDON_DATA_DIR="${ADDON_DATA_DIR:-$REPO_ROOT/tests/logs/addon-test/data}"
ADDON_CONFIG_DIR="${ADDON_CONFIG_DIR:-$REPO_ROOT/tests/logs/addon-test/config}"
ADDON_IMAGE="${ADDON_IMAGE:-bairelay-hassio-test:1.1.0}"

# OS detection: colima only exists on macOS.
case "$(uname -s)" in
	Darwin) IS_MACOS=1 ;;
	Linux)  IS_MACOS=0 ;;
	*)      echo "unsupported OS: $(uname -s) (Linux or macOS required)" >&2; exit 2 ;;
esac

# rtsp://host.docker.internal:<port> is how HA reaches bairelay running
# on the host. macOS: colima resolves host.docker.internal natively.
# Linux: requires `--add-host=host.docker.internal:host-gateway` on the
# HA container (see ha-up.sh's create-once message).
RTSP_HOST="${RTSP_HOST_FROM_HA:-host.docker.internal}"
RTSP_PORT=8554

while [ $# -gt 0 ]; do
	case "$1" in
		-c|--config) CONFIG="$2"; shift 2 ;;
		--no-build) NO_BUILD=1; shift ;;
		--bairelay-as-container) AS_CONTAINER=1; shift ;;
		--keep-colima) KEEP_COLIMA=1; shift ;;
		--help|-h) sed -n '3,50p' "$0"; exit 0 ;;
		*) echo "unknown arg: $1" >&2; exit 2 ;;
	esac
done

mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/ha-verify.log"
BAIRELAY_LOG="$OUT_DIR/bairelay.log"
MAP_FILE="$OUT_DIR/entry_map.json"

log() { printf '[ha-verify] %s\n' "$*" | tee -a "$LOG"; }
fail() { printf '[ha-verify] FAIL: %s\n' "$*" | tee -a "$LOG" >&2; }

PASS=0
FAIL=0
BAIRELAY_PID=""

cleanup() {
	local rc=$?
	if [ "$AS_CONTAINER" = 1 ]; then
		log "stopping bairelay add-on container..."
		docker rm -f bairelay-test >/dev/null 2>&1 || true
	fi
	if [ -n "$BAIRELAY_PID" ] && kill -0 "$BAIRELAY_PID" 2>/dev/null; then
		log "stopping bairelay (pid $BAIRELAY_PID)..."
		kill -INT "$BAIRELAY_PID" 2>/dev/null || true
		wait "$BAIRELAY_PID" 2>/dev/null || true
	fi
	# HA config entries are PERMANENT by policy — never delete them here.
	# HA container stays up for reuse.
	# Colima (macOS only): stop only if we started it this run (marker
	# file present) and the caller didn't pass --keep-colima.
	if [ "$IS_MACOS" = 1 ] && [ "$KEEP_COLIMA" = 0 ] && [ -f "$COLIMA_MARKER" ]; then
		log "ha-up.sh started colima; stopping it (marker removed)"
		colima stop >/dev/null 2>&1 || true
		rm -f "$COLIMA_MARKER"
	fi
	return $rc
}
trap cleanup EXIT INT TERM

# -- Preflight -----------------------------------------------------------

if [ ! -s "$TOKEN_FILE" ]; then
	fail "token file '$TOKEN_FILE' missing or empty — see docs/ha-testing.md"
	exit 2
fi
TOKEN=$(tr -d '\r\n[:space:]' < "$TOKEN_FILE")
if [ -z "$TOKEN" ]; then
	fail "token file '$TOKEN_FILE' is empty after trim"
	exit 2
fi

# Default to the test-rig config (gitignored) so the live config.toml
# stays untouched. If the user passed -c, honour their choice as-is.
if [ -z "$CONFIG" ]; then
	if [ -s "$DEFAULT_TEST_CONFIG" ]; then
		CONFIG="$DEFAULT_TEST_CONFIG"
	else
		fail "tests/bairelay-test.toml missing — copy from tests/bairelay-test.toml.example and fill in camera UIDs/passwords (or pass -c <other>)"
		exit 2
	fi
fi
# Resolve config path relative to repo root if not absolute / not found as-is.
if [ ! -s "$CONFIG" ] && [ -s "$REPO_ROOT/$CONFIG" ]; then
	CONFIG="$REPO_ROOT/$CONFIG"
fi
if [ ! -s "$CONFIG" ]; then
	fail "config '$CONFIG' missing"
	exit 2
fi
log "using config: $CONFIG"

if ! command -v mosquitto_pub >/dev/null 2>&1; then
	if [ "$IS_MACOS" = 1 ]; then
		fail "mosquitto_pub not installed (brew install mosquitto)"
	else
		fail "mosquitto_pub not installed (apt install mosquitto-clients, dnf install mosquitto, etc.)"
	fi
	exit 2
fi

log "ensuring HA is up..."
if ! "$SCRIPT_DIR/ha-up.sh" --wait 60 >>"$LOG" 2>&1; then
	fail "ha-up.sh failed; see $LOG"
	exit 2
fi

# Quick auth check.
CODE=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer $TOKEN" "$HA_URL/api/")
if [ "$CODE" != "200" ]; then
	fail "HA API auth check failed (HTTP $CODE). Token valid? See tests/ha-token"
	exit 2
fi
log "HA API auth OK"

# -- Parse config.toml for cameras + mqtt creds -------------------------

mapfile -t CAM_NAMES < <(grep -E '^\s*\[\[cameras\]\]\s*$' -A 8 "$CONFIG" \
	| awk -F'=' '/^[[:space:]]*name[[:space:]]*=/{gsub(/[" ]/, "", $2); print $2}')

if [ "${#CAM_NAMES[@]}" -eq 0 ]; then
	fail "no [[cameras]] found in $CONFIG"
	exit 2
fi
log "cameras in config: ${CAM_NAMES[*]}"

MQTT_LINE=$(grep -E '^[[:space:]]*credentials' "$CONFIG" | head -1)
MQTT_USER=$(sed -E 's/.*"([^"]+)",\s*"([^"]+)".*/\1/' <<< "$MQTT_LINE")
MQTT_PASS=$(sed -E 's/.*"([^"]+)",\s*"([^"]+)".*/\2/' <<< "$MQTT_LINE")
MQTT_BROKER=$(grep -E '^[[:space:]]*broker_addr' "$CONFIG" | sed -E 's/.*"([^"]+)".*/\1/')
# Test rig moved the broker to port 1884 in 0c20d8d; without an explicit
# `-p`, mosquitto_pub silently defaulted to 1883 and the wake-publish
# silently fell on the floor (Error: Bad file descriptor — connect to a
# nothing-listening port). Read the port from the config so the wake
# publishes actually land.
MQTT_PORT=$(grep -E '^[[:space:]]*port[[:space:]]*=' "$CONFIG" \
	| head -1 | sed -E 's/.*=[[:space:]]*([0-9]+).*/\1/')
if [ -z "$MQTT_PORT" ]; then
	MQTT_PORT=1883
fi

# : mqtt.topic_prefix (default "bairelay"); fall back if absent
# from the config — matches src/config.rs's default_topic_prefix.
MQTT_PREFIX=$(grep -E '^[[:space:]]*topic_prefix' "$CONFIG" | sed -E 's/.*"([^"]+)".*/\1/')
if [ -z "$MQTT_PREFIX" ]; then
	MQTT_PREFIX="bairelay"
fi

if [ -z "$MQTT_BROKER" ] || [ -z "$MQTT_USER" ]; then
	fail "could not parse MQTT broker/credentials from $CONFIG"
	exit 2
fi

# -- Start bairelay ------------------------------------------------------

if [ "$AS_CONTAINER" = 1 ]; then
	[ -f "$ADDON_DATA_DIR/options.json" ] \
		|| { fail "$ADDON_DATA_DIR/options.json missing (need a hand-written Supervisor options.json — see docs/testing.md § HA Add-on verification)"; exit 2; }
	[ -f "$ADDON_CONFIG_DIR/bairelay/config.toml" ] \
		|| { fail "$ADDON_CONFIG_DIR/bairelay/config.toml missing — see docs/testing.md § HA Add-on verification"; exit 2; }

	log "starting bairelay add-on container ($ADDON_IMAGE)..."
	docker rm -f bairelay-test >/dev/null 2>&1 || true
	docker run --rm -d --network host \
		--name bairelay-test \
		-v "$ADDON_DATA_DIR:/data" \
		-v "$ADDON_CONFIG_DIR:/homeassistant_config" \
		"$ADDON_IMAGE" >/dev/null \
		|| { fail "failed to launch add-on container"; exit 2; }
	# Pipe container logs into bairelay.log so the rest of the script
	# (which greps "Startup wake cycle complete") still works.
	docker logs -f bairelay-test > "$BAIRELAY_LOG" 2>&1 &
	BAIRELAY_PID=$!
	log "bairelay container started; log streamer pid $BAIRELAY_PID"
else
	if [ "$NO_BUILD" = 0 ]; then
		log "building bairelay (release)..."
		( cd "$REPO_ROOT" && cargo build --release --bin bairelay ) >>"$LOG" 2>&1 \
			|| { fail "cargo build failed"; exit 2; }
	fi

	log "starting bairelay mqtt-rtsp..."
	# shellcheck disable=SC2086
	RUST_LOG=${RUST_LOG:-bairelay=info,bairelay_rtsp=warn} \
		nohup "$REPO_ROOT/target/release/bairelay" mqtt-rtsp -c "$CONFIG" > "$BAIRELAY_LOG" 2>&1 &
	BAIRELAY_PID=$!
	log "bairelay pid $BAIRELAY_PID"
fi

# Wait for RTSP listener.
for i in $(seq 1 30); do
	if nc -z 127.0.0.1 "$RTSP_PORT" 2>/dev/null; then
		log "RTSP listener up after ${i}s"
		break
	fi
	sleep 1
done
if ! nc -z 127.0.0.1 "$RTSP_PORT" 2>/dev/null; then
	fail "RTSP listener did not open on :$RTSP_PORT"
	exit 2
fi

# Wait for startup-wake complete marker (best-effort, 60s cap).
for i in $(seq 1 60); do
	if grep -q "Startup wake cycle complete" "$BAIRELAY_LOG" 2>/dev/null; then
		log "startup-wake complete after ${i}s"
		break
	fi
	sleep 1
done

# -- Hold wake lock on each camera via MQTT ----------------------------

for cam in "${CAM_NAMES[@]}"; do
	log "MQTT wakeup for $cam (3 min)"
	if ! mosquitto_pub -h "$MQTT_BROKER" -p "$MQTT_PORT" -u "$MQTT_USER" -P "$MQTT_PASS" \
		-t "$MQTT_PREFIX/$cam/control/wakeup" -m 3 >>"$LOG" 2>&1; then
		fail "mqtt publish failed for $cam"
		FAIL=$((FAIL+1))
	fi
done

# Wait for each camera to reach Connected state (best-effort).
log "waiting up to 30 s for cameras to connect..."
for _ in $(seq 1 30); do
	all_connected=1
	for cam in "${CAM_NAMES[@]}"; do
		if ! grep -q "Connected .*camera=$cam" "$BAIRELAY_LOG"; then
			all_connected=0
		fi
	done
	[ "$all_connected" = 1 ] && break
	sleep 1
done

# -- HA REST helpers ----------------------------------------------------

ha_get() {
	curl -s -H "Authorization: Bearer $TOKEN" "$HA_URL$1"
}
ha_post() {
	curl -s -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
		-d "$2" "$HA_URL$1"
}
ha_delete() {
	curl -s -X DELETE -H "Authorization: Bearer $TOKEN" "$HA_URL$1"
}

# Find an existing generic config entry whose stream_source matches $1.
# Prints the entry_id if found; empty otherwise.
#
# HA's REST list endpoint (/api/config/config_entries/entry?domain=generic)
# deliberately strips the `options` field, so we can't match by
# stream_source from REST alone. Read the storage file inside the HA
# container instead — authoritative source of config entry state.
find_entry() {
	local wanted="$1"
	# -i is load-bearing: without it `docker exec` does not pipe our
	# heredoc onto python3's stdin and the script silently reads empty.
	docker exec -i "${HA_CONTAINER:-homeassistant}" python3 - "$wanted" <<'PY'
import json, sys
wanted = sys.argv[1]
data = json.load(open('/config/.storage/core.config_entries'))
for e in data['data']['entries']:
	if e.get('domain') != 'generic':
		continue
	if e.get('options', {}).get('stream_source') == wanted:
		print(e['entry_id'])
		break
PY
}

# Provision a Generic Camera for $1=stream_source. Echoes entry_id on stdout.
provision_entry() {
	local url="$1"
	local flow_id payload rsp entry_id

	# Start flow.
	rsp=$(ha_post "/api/config/config_entries/flow" '{"handler":"generic"}')
	flow_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["flow_id"])' <<< "$rsp")
	if [ -z "$flow_id" ]; then
		fail "could not start generic flow: $rsp"
		return 1
	fi

	# Step 1: stream_source + advanced defaults.
	payload=$(python3 -c 'import json,sys; print(json.dumps({"stream_source":sys.argv[1],"advanced":{"framerate":2,"verify_ssl":False,"rtsp_transport":"tcp"}}))' "$url")
	rsp=$(ha_post "/api/config/config_entries/flow/$flow_id" "$payload")
	if ! grep -q "user_confirm" <<< "$rsp"; then
		fail "flow step 1 did not return user_confirm: $rsp"
		ha_delete "/api/config/config_entries/flow/$flow_id" >/dev/null || true
		return 1
	fi

	# Step 2: confirmed_ok.
	rsp=$(ha_post "/api/config/config_entries/flow/$flow_id" '{"confirmed_ok":true}')
	entry_id=$(python3 -c 'import json,sys; d=json.load(sys.stdin); r=d.get("result",{}); print(r.get("entry_id",""))' <<< "$rsp" 2>/dev/null || true)
	if [ -z "$entry_id" ]; then
		fail "flow create did not return entry_id: $rsp"
		return 1
	fi
	echo "$entry_id"
}

# -- Provision entries --------------------------------------------------

log "ensuring HA Generic Camera entries are present (reuse or create)..."
python3 -c 'import json,sys; json.dump({}, sys.stdout)' > "$MAP_FILE"

STREAMS=()
for cam in "${CAM_NAMES[@]}"; do
	STREAMS+=("$cam:main:rtsp://$RTSP_HOST:$RTSP_PORT/$cam")
	STREAMS+=("$cam:sub:rtsp://$RTSP_HOST:$RTSP_PORT/$cam/sub")
done

for rec in "${STREAMS[@]}"; do
	cam="${rec%%:*}"
	rest="${rec#*:}"
	kind="${rest%%:*}"
	url="${rest#*:}"
	label="$cam/$kind"

	existing=$(find_entry "$url")
	if [ -n "$existing" ]; then
		log "reusing existing HA entry for $label: $existing"
		entry="$existing"
	else
		log "creating permanent HA entry for $label ($url)"
		entry=$(provision_entry "$url") || { FAIL=$((FAIL+1)); continue; }
	fi

	python3 -c "
import json,sys,pathlib
p=pathlib.Path(sys.argv[1])
d=json.loads(p.read_text() or '{}')
d[sys.argv[2]] = {'camera': sys.argv[3], 'kind': sys.argv[4], 'url': sys.argv[5]}
p.write_text(json.dumps(d, indent=2))
" "$MAP_FILE" "$entry" "$cam" "$kind" "$url"
done

# -- Resolve entry_id -> entity_id via HA's /api/template Jinja -------

log "waiting up to 20 s for camera entities to appear..."
for _ in $(seq 1 20); do
	count=$(ha_get "/api/states" | python3 -c 'import json,sys; print(sum(1 for s in json.load(sys.stdin) if s["entity_id"].startswith("camera.")))')
	if [ "$count" -ge "${#STREAMS[@]}" ]; then
		log "camera entities ready ($count)"
		break
	fi
	sleep 1
done

python3 - "$MAP_FILE" "$HA_URL" "$TOKEN" <<'PY' > "$OUT_DIR/resolved.json"
import json, sys, urllib.request

map_path, url, token = sys.argv[1], sys.argv[2], sys.argv[3]
amap = json.load(open(map_path))

def ha_post(path, payload):
	body = json.dumps(payload).encode()
	req = urllib.request.Request(
		url + path, data=body,
		headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
	)
	with urllib.request.urlopen(req, timeout=10) as r:
		return r.read().decode()

tpl = (
	"{% for eid in integration_entities('generic') if eid.startswith('camera.') %}"
	"{{ eid }}|{{ config_entry_id(eid) }}\n"
	"{% endfor %}"
)
rendered = ha_post("/api/template", {"template": tpl}).strip()

resolved = {}
for line in rendered.splitlines():
	line = line.strip()
	if not line or "|" not in line:
		continue
	entity_id, entry_id = line.split("|", 1)
	rec = amap.get(entry_id)
	if not rec:
		continue
	resolved[entity_id] = {
		"entry_id": entry_id,
		"camera": rec["camera"],
		"kind": rec["kind"],
		"url": rec["url"],
	}

json.dump(resolved, sys.stdout, indent=2)
PY

log "entry_id -> camera mapping (before rename):"
cat "$OUT_DIR/resolved.json" | tee -a "$LOG"

# -- Rename config-entry title + entity_id via HA WebSocket --------------
#
# HA's config-flow gives every entry a URL-derived title
# ("host_docker_internal") and the entity_ids differ only by "_2 / _3
# / _4" suffixes — useless in the integrations UI. Fix both properly:
#
#   1. config_entries/update  sets the entry title (shown on the
#      /config/integrations/integration/generic page).
#   2. config/entity_registry/update  sets the entity's canonical
#      name (used for friendly_name) AND its entity_id.
#
# Both calls live behind HA's WebSocket API, not REST, so we exec
# python3 inside the HA container (where the `websockets` package is
# already installed) to drive the connection. Idempotent: if the
# title / entity_id already match the target, skip the update.

RENAME_PAYLOAD=$(python3 - "$OUT_DIR/resolved.json" <<'PY'
import json, re, sys
resolved = json.load(open(sys.argv[1]))
targets = []
for old_eid, info in resolved.items():
	cam = info["camera"]
	kind = info["kind"]
	# Slugify camera name for a Python-identifier-safe entity_id suffix.
	slug = re.sub(r'[^a-z0-9]+', '_', cam.lower()).strip('_')
	new_eid = f"camera.bairelay_{slug}_{kind}"
	title = f"{cam} ({kind}) [bairelay]"
	targets.append({
		"entry_id": info["entry_id"],
		"old_entity_id": old_eid,
		"new_entity_id": new_eid,
		"title": title,
		"name": title,
	})
print(json.dumps(targets))
PY
)

log "renaming HA entries + entity_ids via WebSocket..."
docker exec -i "${HA_CONTAINER:-homeassistant}" python3 - "$TOKEN" <<PY >> "$LOG" 2>&1
import asyncio, json, sys
import websockets

TOKEN = sys.argv[1]
TARGETS = json.loads('''$RENAME_PAYLOAD''')

NEXT_ID = 0
def nid():
	global NEXT_ID
	NEXT_ID += 1
	return NEXT_ID

async def call(ws, msg):
	msg["id"] = nid()
	await ws.send(json.dumps(msg))
	while True:
		r = json.loads(await ws.recv())
		if r.get("id") == msg["id"]:
			return r

async def main():
	async with websockets.connect("ws://localhost:8123/api/websocket") as ws:
		await ws.recv()  # auth_required
		await ws.send(json.dumps({"type": "auth", "access_token": TOKEN}))
		auth_ok = json.loads(await ws.recv())
		assert auth_ok.get("type") == "auth_ok", auth_ok

		# Current entry titles.
		cur_entries = await call(ws, {"type": "config_entries/get", "domain": "generic"})
		cur_title = {e["entry_id"]: e["title"] for e in cur_entries.get("result", [])}

		# Current entity registry snapshot (entity_id -> name).
		cur_reg = await call(ws, {"type": "config/entity_registry/list"})
		cur_name = {e["entity_id"]: e.get("name") for e in cur_reg.get("result", [])}

		# Device registry snapshot (entry_id -> device_id, name_by_user).
		dev_reg = await call(ws, {"type": "config/device_registry/list"})
		entry_to_device = {}
		cur_dev_name = {}
		for d in dev_reg.get("result", []):
			for eid in d.get("config_entries", []):
				entry_to_device.setdefault(eid, d["id"])
			cur_dev_name[d["id"]] = d.get("name_by_user")

		for t in TARGETS:
			# 1. entry title.
			if cur_title.get(t["entry_id"]) != t["title"]:
				r = await call(ws, {
					"type": "config_entries/update",
					"entry_id": t["entry_id"],
					"title": t["title"],
				})
				print(f"title  {t['entry_id']} -> {t['title']}: success={r.get('success')}")
			else:
				print(f"title  {t['entry_id']} already set")

			# 2. device name_by_user (Settings > Devices view).
			dev_id = entry_to_device.get(t["entry_id"])
			if dev_id:
				if cur_dev_name.get(dev_id) != t["title"]:
					r = await call(ws, {
						"type": "config/device_registry/update",
						"device_id": dev_id,
						"name_by_user": t["title"],
					})
					print(f"device {dev_id} -> {t['title']}: success={r.get('success')}")
				else:
					print(f"device {dev_id} already set")
			else:
				print(f"device  entry {t['entry_id']} has no associated device yet")

			# 3. entity registry: rename entity_id + set name.
			needs_update = (
				t["old_entity_id"] != t["new_entity_id"]
				or cur_name.get(t["old_entity_id"]) != t["name"]
			)
			if needs_update:
				r = await call(ws, {
					"type": "config/entity_registry/update",
					"entity_id": t["old_entity_id"],
					"new_entity_id": t["new_entity_id"],
					"name": t["name"],
				})
				print(f"entity {t['old_entity_id']} -> {t['new_entity_id']}: success={r.get('success')}")
			else:
				print(f"entity {t['new_entity_id']} already set")

asyncio.run(main())
PY

# Re-resolve entity_ids after rename (mapping file now points at the
# new entity_ids so the snapshot loop uses the correct URLs).
python3 - "$MAP_FILE" "$HA_URL" "$TOKEN" <<'PY' > "$OUT_DIR/resolved.json"
import json, sys, urllib.request

map_path, url, token = sys.argv[1], sys.argv[2], sys.argv[3]
amap = json.load(open(map_path))

def ha_post(path, payload):
	body = json.dumps(payload).encode()
	req = urllib.request.Request(
		url + path, data=body,
		headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
	)
	with urllib.request.urlopen(req, timeout=10) as r:
		return r.read().decode()

tpl = (
	"{% for eid in integration_entities('generic') if eid.startswith('camera.') %}"
	"{{ eid }}|{{ config_entry_id(eid) }}\n"
	"{% endfor %}"
)
rendered = ha_post("/api/template", {"template": tpl}).strip()
resolved = {}
for line in rendered.splitlines():
	line = line.strip()
	if not line or "|" not in line:
		continue
	entity_id, entry_id = line.split("|", 1)
	rec = amap.get(entry_id)
	if not rec:
		continue
	resolved[entity_id] = {
		"entry_id": entry_id,
		"camera": rec["camera"],
		"kind": rec["kind"],
		"url": rec["url"],
	}
json.dump(resolved, sys.stdout, indent=2)
PY

log "entity_id -> camera mapping (after rename):"
cat "$OUT_DIR/resolved.json" | tee -a "$LOG"

# -- Fetch snapshots ----------------------------------------------------
#
# Two paths per stream (see docs/ha-testing.md §7 for rationale):
#   1. "ha"   -- HA /api/camera_proxy/<entity>. Generic Camera wraps the
#                URL with ffmpeg: when registering in go2rtc. Must pass
#                for every stream including HEVC main.
#   2. "g2r"  -- direct go2rtc with a raw RTSP URL. Must pass for every
#                stream; failure here is a real bairelay regression.

GO_SOCK=$(docker exec "${HA_CONTAINER:-homeassistant}" sh -c 'ls -t /tmp/go2rtc-*/go2rtc.sock 2>/dev/null | head -1' | tr -d '\r\n')
GO_YAML=$(docker exec "${HA_CONTAINER:-homeassistant}" sh -c 'ls -t /tmp/go2rtc-*/go2rtc_*.yaml 2>/dev/null | head -1' | tr -d '\r\n')
GO_USER=""
GO_PASS=""
if [ -n "$GO_YAML" ]; then
	GO_USER=$(docker exec "${HA_CONTAINER:-homeassistant}" sh -c "grep '^  username:' $GO_YAML | awk '{print \$2}'" | tr -d '\r\n')
	GO_PASS=$(docker exec "${HA_CONTAINER:-homeassistant}" sh -c "grep '^  password:' $GO_YAML | awk '{print \$2}'" | tr -d '\r\n')
fi
if [ -z "$GO_SOCK" ] || [ -z "$GO_USER" ]; then
	log "WARN could not locate go2rtc socket/credentials — skipping go2rtc-native checks"
	GO_SOCK=""
fi

g2r_snapshot() {
	# $1 = stream name, $2 = rtsp URL, $3 = output jpeg
	local name="$1" url="$2" out="$3"
	docker exec "${HA_CONTAINER:-homeassistant}" sh -c \
		"curl -s -u $GO_USER:$GO_PASS -X PUT --unix-socket $GO_SOCK 'http://localhost/api/streams?name=$name&src=$url' -o /dev/null -w '%{http_code}\n'" \
		>/dev/null || true
	docker exec "${HA_CONTAINER:-homeassistant}" sh -c \
		"curl -s -u $GO_USER:$GO_PASS --unix-socket $GO_SOCK 'http://localhost/api/frame.jpeg?src=$name' --max-time 30" \
		> "$out" 2>/dev/null || true
	local rc=$?
	docker exec "${HA_CONTAINER:-homeassistant}" sh -c \
		"curl -s -u $GO_USER:$GO_PASS -X DELETE --unix-socket $GO_SOCK 'http://localhost/api/streams?src=$name' -o /dev/null" \
		>/dev/null 2>&1 || true
	return "$rc"
}

log ""
log "fetching snapshots (two paths per stream)..."

TOTAL=0

while IFS= read -r ent; do
	TOTAL=$((TOTAL+1))
	info=$(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); e=sys.argv[2]; v=d[e]; print(v["camera"], v["kind"], v["url"])' "$OUT_DIR/resolved.json" "$ent")
	cam=$(awk '{print $1}' <<< "$info")
	kind=$(awk '{print $2}' <<< "$info")
	url=$(awk '{print $3}' <<< "$info")
	label="$cam/$kind"

	# Path 1: HA /api/camera_proxy.
	ha_jpeg="$OUT_DIR/ha-${ent#camera.}.jpg"
	http_code=$(curl -s -o "$ha_jpeg" -w '%{http_code}' \
		-H "Authorization: Bearer $TOKEN" \
		--max-time 30 \
		"$HA_URL/api/camera_proxy/$ent" || echo 000)
	size=$(wc -c < "$ha_jpeg" 2>/dev/null | tr -d ' ' || echo 0)
	[ -z "$size" ] && size=0
	magic=$(head -c 3 "$ha_jpeg" 2>/dev/null | xxd -p 2>/dev/null || echo "")

	if [ "$http_code" = "200" ] && [ "$size" -gt 1024 ] && [ "$magic" = "ffd8ff" ]; then
		log "PASS  ha/camera_proxy/$label  entity=$ent  size=${size}B"
		PASS=$((PASS+1))
	else
		fail "ha/camera_proxy/$label  entity=$ent  size=${size}B http=$http_code magic=$magic"
		FAIL=$((FAIL+1))
	fi

	# Path 2: go2rtc native.
	if [ -n "$GO_SOCK" ]; then
		TOTAL=$((TOTAL+1))
		g2r_jpeg="$OUT_DIR/g2r-${ent#camera.}.jpg"
		stream_name="bairelay_${cam}_${kind}"
		g2r_snapshot "$stream_name" "$url" "$g2r_jpeg" || true
		size=$(wc -c < "$g2r_jpeg" 2>/dev/null | tr -d ' ' || echo 0)
		[ -z "$size" ] && size=0
		magic=$(head -c 3 "$g2r_jpeg" 2>/dev/null | xxd -p 2>/dev/null || echo "")
		if [ "$size" -gt 1024 ] && [ "$magic" = "ffd8ff" ]; then
			log "PASS  g2r-native/$label                  size=${size}B"
			PASS=$((PASS+1))
		else
			fail "g2r-native/$label  size=${size}B magic=$magic"
			FAIL=$((FAIL+1))
		fi
	fi
done < <(python3 -c 'import json,sys; [print(k) for k in json.load(open(sys.argv[1])).keys()]' "$OUT_DIR/resolved.json")

# -- Summary ------------------------------------------------------------

log ""
log "======================================================================"
log "summary"
log "======================================================================"
log "passed:       $PASS / $TOTAL"
log "failed:       $FAIL"
log "logs:         $OUT_DIR/"

if [ "$FAIL" -gt 0 ] || [ "$PASS" -eq 0 ]; then
	exit 1
fi
exit 0
