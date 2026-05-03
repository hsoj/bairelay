#!/usr/bin/env bash
# scripts/release.sh — cut a new bairelay release.
#
#   1. Validates args, branch (main), tracked-tree cleanliness, tag uniqueness.
#   2. Seeds a new CHANGELOG section with commits since the last v* tag,
#      opens $EDITOR (falling back to $VISUAL, then `vi`) for review.
#   3. Bumps `[workspace.package].version` in Cargo.toml; refreshes Cargo.lock.
#   4. Shows the full diff; on confirmation commits + tags vX.Y.Z.
#   5. On a second confirmation pushes main + the tag to origin.
#
# The release.yml workflow then takes over: matrix-builds the binary
# for x86_64-linux-musl, aarch64-linux-musl, aarch64-darwin, and
# x86_64-windows-msvc, packages each with README/LICENSE/CHANGELOG/
# sample_config, and posts a draft GitHub Release.

set -euo pipefail

usage() {
	cat >&2 <<'EOF'
Usage: scripts/release.sh <version>

Arguments:
    <version>   semver MAJOR.MINOR.PATCH (e.g. 0.10.0)

Environment:
    EDITOR      editor for CHANGELOG review (default: ${VISUAL:-vi})
EOF
	exit 2
}

die() {
	echo "error: $*" >&2
	exit 1
}

confirm() {
	local prompt="$1"
	local ans=""
	read -r -p "$prompt [y/N] " ans
	[[ "$ans" =~ ^[yY] ]]
}

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

update_workspace_version() {
	local new_v="$1"
	awk -v new_v="$new_v" '
		/^\[workspace\.package\]/ { in_section = 1; print; next }
		/^\[/                     { in_section = 0 }
		in_section && /^version[[:space:]]*=/ {
			print "version = \"" new_v "\""
			next
		}
		{ print }
	' Cargo.toml > Cargo.toml.new
	mv Cargo.toml.new Cargo.toml
}

build_new_section() {
	local version="$1"
	local today="$2"
	local range="$3"

	cat <<EOF
## [$version] — $today

<!-- Edit this section: organise commits into thematic subsections, -->
<!-- tighten language, drop noise. Single-line HTML comments are -->
<!-- stripped before commit. Empty section = abort. -->

### Changes

EOF
	git log --no-merges --pretty=format:'- %s' "$range"
	printf '\n\n'
}

insert_changelog_section() {
	# Pure-shell insert before the first `## [` heading. Avoids awk's
	# `-v section=...` form, which BSD awk on macOS rejects for multi-
	# line values with `awk: newline in string ... at source line 1`.
	#
	# `$1` arrives stripped of trailing newlines (a `$(...)` rule), so
	# append a final blank line ourselves — otherwise the new section's
	# last bullet collides with the next section's `## [` heading.
	local section="$1"$'\n\n'
	local insert_line
	insert_line="$(grep -n '^## \[' CHANGELOG.md | head -1 | cut -d: -f1 || true)"
	if [[ -z "$insert_line" ]]; then
		printf '%s' "$section" >> CHANGELOG.md
		return
	fi
	{
		head -n "$((insert_line - 1))" CHANGELOG.md
		printf '%s' "$section"
		tail -n "+$insert_line" CHANGELOG.md
	} > CHANGELOG.md.new
	mv CHANGELOG.md.new CHANGELOG.md
}

strip_placeholder_comments() {
	awk '!/^[[:space:]]*<!--.*-->[[:space:]]*$/' CHANGELOG.md > CHANGELOG.md.new
	mv CHANGELOG.md.new CHANGELOG.md
}

new_section_is_empty() {
	local version="$1"
	local body
	body="$(awk -v ver="$version" '
		$0 ~ "^## \\[" ver "\\]"   { capture = 1; next }
		capture && /^## \[/        { exit }
		capture && !/^[[:space:]]*<!--.*-->[[:space:]]*$/ { print }
	' CHANGELOG.md)"
	[[ -z "$(printf '%s' "$body" | tr -d '[:space:]')" ]]
}

main() {
	[[ $# -eq 1 ]] || usage
	local new_version="$1"
	[[ "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
		|| die "Invalid version: '$new_version' (expected MAJOR.MINOR.PATCH)"

	cd "$(git rev-parse --show-toplevel)"

	[[ "$(git rev-parse --abbrev-ref HEAD)" == "main" ]] \
		|| die "must be on branch 'main'"
	git diff-index --quiet HEAD -- \
		|| die "working tree has uncommitted tracked changes; commit or stash first"

	local tag="v$new_version"
	if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
		die "tag $tag already exists"
	fi

	local current
	current="$(read_workspace_version)"
	[[ -n "$current" ]] || die "could not read [workspace.package].version from Cargo.toml"
	[[ "$current" != "$new_version" ]] \
		|| die "Cargo.toml is already at $new_version"

	echo "Bumping $current → $new_version (tag $tag)"
	echo

	local last_tag commit_range
	last_tag="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)"
	if [[ -n "$last_tag" ]]; then
		commit_range="$last_tag..HEAD"
		echo "Commits since $last_tag will seed the CHANGELOG draft."
	else
		commit_range="HEAD"
		echo "No prior v* tag; full history will seed the CHANGELOG draft."
	fi
	echo

	local today
	today="$(date +%Y-%m-%d)"

	local new_section
	new_section="$(build_new_section "$new_version" "$today" "$commit_range")"
	insert_changelog_section "$new_section"

	local editor="${EDITOR:-${VISUAL:-vi}}"
	echo "Opening CHANGELOG.md in $editor..."
	"$editor" CHANGELOG.md

	if new_section_is_empty "$new_version"; then
		git checkout -- CHANGELOG.md
		die "new section for $new_version is empty after edit; aborted"
	fi

	strip_placeholder_comments

	echo
	echo "=== CHANGELOG.md diff ==="
	git --no-pager diff CHANGELOG.md
	echo
	confirm "Continue with version bump and Cargo.lock refresh?" \
		|| { git checkout -- CHANGELOG.md; die "aborted before version bump"; }

	update_workspace_version "$new_version"

	echo
	echo "Refreshing Cargo.lock..."
	cargo check --workspace --quiet

	echo
	echo "=== full diff (Cargo.toml + Cargo.lock + CHANGELOG.md) ==="
	git --no-pager diff Cargo.toml Cargo.lock CHANGELOG.md
	echo
	confirm "Commit and tag $tag?" || {
		git checkout -- Cargo.toml Cargo.lock CHANGELOG.md
		die "aborted before commit"
	}

	git add Cargo.toml Cargo.lock CHANGELOG.md
	git commit -m "release: $tag"
	git tag -a "$tag" -m "Release $tag"
	echo
	git --no-pager log -1 --stat
	echo

	if confirm "Push main + $tag to origin?"; then
		git push origin main
		git push origin "$tag"
		echo
		echo "Pushed. Watch the release workflow at:"
		echo "  https://github.com/mgc8/bairelay/actions/workflows/release.yml"
		echo
		echo "Once green, review and publish the draft release at:"
		echo "  https://github.com/mgc8/bairelay/releases"
		echo
		echo "Reminder: if you ran this from the public clone, mirror the"
		echo "tag on the internal repo so private history carries the same"
		echo "release marker. From the internal clone:"
		echo "    git checkout main && git pull"
		echo "    git tag -a $tag -m 'Release $tag'"
		echo "    git push origin $tag"
	else
		echo
		echo "Stopped before push. To push later:"
		echo "  git push origin main && git push origin $tag"
		echo "To abort entirely:"
		echo "  git tag -d $tag && git reset --hard HEAD~1"
	fi
}

trap 'rm -f Cargo.toml.new CHANGELOG.md.new' EXIT

main "$@"
