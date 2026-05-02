## Summary

<!-- One paragraph: what this PR does and why. -->

## Changes

<!-- Bullet list of the load-bearing changes. -->

-
-

## Testing

<!--
How did you validate this? Workspace gates are required:
  cargo fmt --all
  cargo clippy --all-targets -- -D warnings
  cargo test
RTSP-adjacent changes also need live-verify against real camera
hardware (`tests/scripts/manual-verify.sh`); HA-adjacent changes
need `tests/scripts/ha-verify.sh` 8/8.
-->

- [ ] `cargo fmt --all` ran clean
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes
- [ ] Live-verify on real camera hardware (RTSP / HA changes only)

## Definition of done

- [ ] Tests cover the change (unit + integration as applicable)
- [ ] Documentation updated (`docs/architecture.md`, `docs/implementation.md`, `README.md`, `CHANGELOG.md`)
- [ ] No `TODO` / `FIXME` comments left without an owner
- [ ] Configuration changes documented in `sample_config.toml` with a 1–3 line comment

## Notes for reviewers

<!-- Anything reviewers should know up-front: tricky tradeoffs, follow-ups planned, related issues. -->
