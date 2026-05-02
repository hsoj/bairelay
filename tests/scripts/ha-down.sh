#!/usr/bin/env bash
# Tear down the Home Assistant test rig.
#
# Stops the HA + mosquitto containers. On macOS, if
# tests/logs/colima.started-by-us exists (meaning tests/scripts/ha-up.sh
# started colima during a prior run), also stops colima and removes the
# marker. A colima that was already running when ha-up.sh ran is NOT
# touched — it may be in use by other projects. On Linux there is no
# colima to manage; --colima-only is a no-op.
#
# Usage:
#     tests/scripts/ha-down.sh                 # stop HA + colima-if-we-started-it
#     tests/scripts/ha-down.sh --ha-only       # only stop HA; leave colima
#     tests/scripts/ha-down.sh --colima-only   # only handle colima; leave HA
#
# Exit codes:
#     0  clean shutdown
#     1  error

set -u
set -o pipefail

CONTAINER_NAME="${HA_CONTAINER:-homeassistant}"
MQTT_CONTAINER="${MQTT_CONTAINER:-bairelay-mosquitto}"
STOP_HA=1
STOP_COLIMA=1

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd)
COLIMA_MARKER="$REPO_ROOT/tests/logs/colima.started-by-us"

# OS detection: colima only exists on macOS.
case "$(uname -s)" in
	Darwin) IS_MACOS=1 ;;
	Linux)  IS_MACOS=0 ;;
	*)      echo "unsupported OS: $(uname -s) (Linux or macOS required)" >&2; exit 1 ;;
esac

while [ $# -gt 0 ]; do
	case "$1" in
		--ha-only) STOP_COLIMA=0; shift ;;
		--colima-only) STOP_HA=0; shift ;;
		--help|-h) sed -n '3,20p' "$0"; exit 0 ;;
		*) echo "unknown arg: $1" >&2; exit 2 ;;
	esac
done

log() { printf '[ha-down] %s\n' "$*"; }

if [ "$STOP_HA" = 1 ]; then
	for c in "$CONTAINER_NAME" "$MQTT_CONTAINER"; do
		if docker inspect "$c" >/dev/null 2>&1; then
			STATE=$(docker inspect -f '{{.State.Status}}' "$c")
			if [ "$STATE" = "running" ]; then
				log "stopping container '$c'..."
				docker stop "$c" >/dev/null
			else
				log "container '$c' not running (state=$STATE); nothing to do"
			fi
		else
			log "container '$c' not found; nothing to do"
		fi
	done
fi

if [ "$STOP_COLIMA" = 1 ] && [ "$IS_MACOS" = 1 ]; then
	if [ -f "$COLIMA_MARKER" ]; then
		log "marker $COLIMA_MARKER present — ha-up.sh started colima; stopping"
		if colima status >/dev/null 2>&1; then
			colima stop
		else
			log "colima already stopped"
		fi
		rm -f "$COLIMA_MARKER"
	else
		log "no colima marker; leaving colima alone"
	fi
fi

log "done"
