# Live-camera fixtures

Hand-captured artifacts from real Argus cameras. The whole directory is gitignored except this file — fixtures are per-camera, may carry privacy-sensitive content, and drift with firmware. Recapture on demand.

## BcMedia stream dumps

Replay material for the translator + RTSP-server path:

	cargo run -- mqtt-rtsp --dump-bcmedia tests/fixtures -c config.toml

Each stream produces `<cam>-<kind>.bcmedia` and a sibling `<cam>-<kind>.meta.json`. `tests/fixture_replay.rs::fake_provider_replays_real_fixtures_if_present` skips when no `.bcmedia` files are present and asserts H.264 / H.265 with matching parameter sets when they are.

## abilityInfo XML

Ground-truth `<AbilityInfo>` per camera — reference data for `MissingAbility` gate decisions on `email.rs` / `services.rs` / `users.rs` / `pushinfo.rs`:

	cargo run --release -- abilities <CameraName> -c config.toml --json | jq -r .xml > tests/fixtures/<CameraName>.xml

`<CameraName>.xml` is the one-line re-serialised XML the camera replied with. `bairelay abilities` (no `--json`) also prints a parsed `(module, name, kind)` table alongside the XML for quick human inspection.
