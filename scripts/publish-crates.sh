#!/usr/bin/env bash
#
# publish-crates.sh — cargo publish for bairelay.
#
# Usage:
#   scripts/publish-crates.sh                # interactive, real publish
#   scripts/publish-crates.sh --dry-run      # cargo publish --dry-run, no upload
#   scripts/publish-crates.sh --yes          # non-interactive (CI mode)
#
# There is one crate. This used to orchestrate five in dependency
# strata, with sleeps between them so the crates.io sparse index caught
# up; merging the libraries into `bairelay` removed the ordering problem
# along with the crates.
#
# Prerequisites:
#   - `cargo login` already done, OR CARGO_REGISTRY_TOKEN env var set.
#   - Working tree clean against the tag you intend to publish.
#
# Exit codes:
#   0  — published (or dry-ran) successfully.
#   1  — generic failure (publish error, dirty tree, etc).
#   2  — argument / environment error.

set -euo pipefail

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

# Version read from [package].version in the single-crate manifest.
read_package_version() {
	awk '
		/^\[package\]/ { in_section = 1; next }
		/^\[/           { in_section = 0 }
		in_section && /^version[[:space:]]*=/ {
			match($0, /"[^"]*"/)
			print substr($0, RSTART + 1, RLENGTH - 2)
			exit
		}
	' Cargo.toml
}

VERSION="$(read_package_version)"
[[ -n "$VERSION" ]] || die "could not read [package].version from Cargo.toml"

ACTION="publish"
[[ $DRY_RUN -eq 1 ]] && ACTION="dry-run"

echo "Publishing bairelay v${VERSION} to crates.io ($ACTION)"
echo

# Sanity bound: the packaged crate is source + tests + operator docs.
# A sudden jump means something (a fixture tree, a scratch dir) slipped
# past `[package].exclude`.
MAX_BINARY_FILES=400

echo "=== Pre-flight: package contents ==="
listing="$(cargo package --list)" || die "cargo package --list failed"
count="$(printf '%s\n' "$listing" | wc -l | tr -d ' ')"
[[ $count -gt 0 ]] || die "cargo package --list returned no files"
echo "    bairelay: $count files"
if [[ $count -gt $MAX_BINARY_FILES ]]; then
	die "bairelay package has $count files (> $MAX_BINARY_FILES) — abort"
fi
if printf '%s\n' "$listing" | grep -qE "plans"; then
	die "bairelay package contains files matching 'plans' — abort"
fi
echo

confirm "Proceed?" || die "aborted"

echo
echo "  → ${ACTION}: bairelay"
if [[ $DRY_RUN -eq 1 ]]; then
	cargo publish --dry-run --quiet
else
	cargo publish --quiet
fi
echo "    ✓ bairelay"

echo
if [[ $DRY_RUN -eq 1 ]]; then
	echo "Dry-run complete for v${VERSION}. No uploads made."
else
	echo "Published bairelay v${VERSION} to crates.io. View at:"
	echo "  https://crates.io/crates/bairelay/${VERSION}"
fi
