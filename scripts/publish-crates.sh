#!/usr/bin/env bash
#
# publish-crates.sh — bottom-up cargo publish for the five bairelay
# crates.
#
# Usage:
#   scripts/publish-crates.sh                # interactive, real publish
#   scripts/publish-crates.sh --dry-run      # cargo publish --dry-run, no upload
#   scripts/publish-crates.sh --yes          # non-interactive (CI mode)
#
# Order is dependency-bottom-up. Three leaf crates may run in parallel
# (no internal deps); the two downstream crates wait. Between strata
# we sleep PROPAGATE_SECS (default 30) so the crates.io sparse index
# catches up before the next stratum resolves its deps.
#
# Prerequisites:
#   - `cargo login` already done, OR CARGO_REGISTRY_TOKEN env var set.
#   - Working tree clean against the tag you intend to publish.
#
# Exit codes:
#   0  — all five crates published (or dry-ran) successfully.
#   1  — generic failure (publish error, dirty tree, etc).
#   2  — argument / environment error.

set -euo pipefail

# Dependency order. Stratum 1 (leaves) runs in parallel; strata 2 and
# 3 sequentially after their deps land on crates.io.
LEAVES=(bairelay-neolink-core bairelay-mqtt bairelay-rtsp)
STRATUM_2=(bairelay-wake-server)
STRATUM_3=(bairelay)

PROPAGATE_SECS="${PROPAGATE_SECS:-30}"

DRY_RUN=0
YES=0

die() {
	echo "error: $*" >&2
	exit 1
}

usage() {
	sed -n '2,/^$/p' "$0" >&2
	exit 2
}

confirm() {
	local prompt="$1"
	if [[ $YES -eq 1 ]]; then
		return 0
	fi
	local ans=""
	read -r -p "$prompt [y/N] " ans
	[[ "$ans" =~ ^[yY] ]]
}

while [[ $# -gt 0 ]]; do
	case "$1" in
		--dry-run) DRY_RUN=1 ;;
		--yes)     YES=1 ;;
		-h|--help) usage ;;
		*)         echo "unknown arg: $1" >&2; usage ;;
	esac
	shift
done

cd "$(git rev-parse --show-toplevel)"

# Clean tracked tree — same gate as release.sh.
git diff-index --quiet HEAD -- \
	|| die "working tree has uncommitted tracked changes; commit or stash first"

# `cargo publish --dry-run` doesn't require auth; the real publish does.
if [[ $DRY_RUN -eq 0 ]]; then
	if [[ -z "${CARGO_REGISTRY_TOKEN:-}" ]]; then
		# `cargo login` writes to ~/.cargo/credentials.toml; presence
		# of a `[registries.crates-io]` block (or legacy `[registry]`)
		# is the indicator.
		creds="${CARGO_HOME:-$HOME/.cargo}/credentials.toml"
		if [[ ! -f "$creds" ]] || ! grep -q '^\[' "$creds"; then
			die "no crates.io credentials found. Run 'cargo login' or export CARGO_REGISTRY_TOKEN."
		fi
	fi
fi

# Version derived from the workspace manifest. All five crates inherit
# from [workspace.package].version via `version.workspace = true`, so a
# single read suffices.
read_workspace_version() {
	awk '
		/^\[workspace\.package\]/ { in_section = 1; next }
		/^\[/                     { in_section = 0 }
		in_section && /^version[[:space:]]*=/ {
			match($0, /"[^"]*"/)
			print substr($0, RSTART + 1, RLENGTH - 2)
			exit
		}
	' Cargo.toml
}

VERSION="$(read_workspace_version)"
[[ -n "$VERSION" ]] || die "could not read [workspace.package].version from Cargo.toml"

ACTION="publish"
[[ $DRY_RUN -eq 1 ]] && ACTION="dry-run"

echo "Publishing bairelay v${VERSION} to crates.io ($ACTION)"
echo "  stratum 1 (leaves, parallel): ${LEAVES[*]}"
echo "  stratum 2:                    ${STRATUM_2[*]}"
echo "  stratum 3:                    ${STRATUM_3[*]}"
echo "  sleep between strata:         ${PROPAGATE_SECS}s"
echo

# Sanity bound
MAX_BINARY_FILES=100

echo "=== Pre-flight: package contents ==="
for crate in "${LEAVES[@]}" "${STRATUM_2[@]}" "${STRATUM_3[@]}"; do
	listing="$(cargo package --list -p "$crate")" \
		|| die "cargo package --list failed for $crate"
	count="$(printf '%s\n' "$listing" | wc -l | tr -d ' ')"
	[[ $count -gt 0 ]] || die "cargo package --list returned no files for $crate"
	echo "    $crate: $count files"
	if [[ "$crate" == "bairelay" ]]; then
		if [[ $count -gt $MAX_BINARY_FILES ]]; then
			die "bairelay package has $count files (> $MAX_BINARY_FILES) — abort"
		fi
		if printf '%s\n' "$listing" | grep -qE "plans"; then
			die "bairelay package contains files matching 'plans' — abort"
		fi
	fi
done
echo

confirm "Proceed?" || die "aborted"

publish_one() {
	local crate="$1"
	echo "  → ${ACTION}: $crate"
	if [[ $DRY_RUN -eq 1 ]]; then
		cargo publish --dry-run -p "$crate" --quiet
	else
		cargo publish -p "$crate" --quiet
	fi
}

# Stratum 1: leaves in parallel.
echo
echo "=== Stratum 1 (parallel) ==="
pids=()
for crate in "${LEAVES[@]}"; do
	publish_one "$crate" &
	pids+=("$!:$crate")
done
fail=0
for entry in "${pids[@]}"; do
	pid="${entry%%:*}"
	crate="${entry##*:}"
	if ! wait "$pid"; then
		echo "    × $crate failed" >&2
		fail=1
	else
		echo "    ✓ $crate"
	fi
done
[[ $fail -eq 0 ]] || die "stratum 1 had failures; aborting before stratum 2"

# Index propagation gap. Sparse-index catches up within seconds on
# crates.io but the contract isn't published; 30s is the commonly
# cited safe number.
if [[ $DRY_RUN -eq 0 ]]; then
	echo
	echo "Sleeping ${PROPAGATE_SECS}s for index propagation…"
	sleep "$PROPAGATE_SECS"
fi

# Stratum 2.
echo
echo "=== Stratum 2 ==="
for crate in "${STRATUM_2[@]}"; do
	publish_one "$crate"
	echo "    ✓ $crate"
done

if [[ $DRY_RUN -eq 0 ]]; then
	echo
	echo "Sleeping ${PROPAGATE_SECS}s for index propagation…"
	sleep "$PROPAGATE_SECS"
fi

# Stratum 3 (the binary).
echo
echo "=== Stratum 3 ==="
for crate in "${STRATUM_3[@]}"; do
	publish_one "$crate"
	echo "    ✓ $crate"
done

echo
if [[ $DRY_RUN -eq 1 ]]; then
	echo "Dry-run complete for v${VERSION}. No uploads made."
else
	echo "Published bairelay v${VERSION} to crates.io. View at:"
	echo "  https://crates.io/crates/bairelay/${VERSION}"
fi
