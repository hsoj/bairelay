# tests/logs

Transient output from `tests/scripts/*.sh` lives here. Everything except this README is gitignored (see `.gitignore`).

## Expected contents

- `ha-verify/` — output of `tests/scripts/ha-verify.sh`:
  - `ha-verify.log` — the run log.
  - `bairelay.log` — stdout/stderr from the bairelay process spawned for this run.
  - `entry_map.json` — (entry_id → camera/kind/url) mapping recorded during HA config-flow provisioning.
  - `resolved.json` — (entity_id → entry_id/camera/kind/url) mapping after HA registers the entities.
  - `ha-<entity>.jpg` — snapshot fetched via HA /api/camera_proxy.
  - `g2r-<entity>.jpg` — snapshot fetched via go2rtc native RTSP.

- `colima.started-by-us` — marker file written by `tests/scripts/ha-up.sh` when it started colima. Presence means "we started colima; we should stop it on teardown". `tests/scripts/ ha-down.sh` and `tests/scripts/ha-verify.sh` remove the marker after stopping colima. A pre-existing colima (no marker) is never touched.

See `docs/ha-testing.md` for the full workflow.
