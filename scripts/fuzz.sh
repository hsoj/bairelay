#!/usr/bin/env bash
# Run every fuzz target (or the ones named on the command line) for
# FUZZ_TIME seconds each. libfuzzer is loud — its per-iteration progress
# lines, "NEW", "REDUCE", "pulse" output is signal only when chasing a
# specific corpus issue, so this wrapper buries it in fuzz/logs/<target>.log
# and surfaces just the verdict per target:
#
#   <target>   OK    (<execs> execs / <secs>s)
#   <target>   CRASH — see fuzz/logs/<target>.log
#       <panic line>
#       Test unit written to ./artifacts/<target>/<crash-file>
#
# Exit code is non-zero iff any target crashed.
#
# Usage:
#   scripts/fuzz.sh                       # every target, 10s each
#   scripts/fuzz.sh aac_parse_adts        # one target only
#   FUZZ_TIME=120 scripts/fuzz.sh         # longer window

set -uo pipefail

FUZZ_TIME="${FUZZ_TIME:-10}"
ROOT="$(git rev-parse --show-toplevel)"
LOG_DIR="$ROOT/fuzz/logs"
mkdir -p "$LOG_DIR"

cd "$ROOT/fuzz"

# Pass the real host triple explicitly. cargo-fuzz's default --target is
# the triple cargo-fuzz ITSELF was compiled for — a prebuilt musl-static
# cargo-fuzz (e.g. from taiki-e/install-action in CI) therefore defaults
# to x86_64-unknown-linux-musl, where ASan is incompatible with the
# statically linked libc and the build fails. rustc's own host triple is
# authoritative on every machine regardless of how cargo-fuzz was built.
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"

if [ "$#" -gt 0 ]; then
	targets="$*"
else
	targets="$(cargo fuzz list)"
fi

if [ -z "$targets" ]; then
	echo "no fuzz targets found (toolchain set up? cargo fuzz list returned empty)" >&2
	exit 1
fi

build_log="$LOG_DIR/build.log"
printf 'building fuzz targets... '
build_start=$SECONDS
if ! cargo fuzz build --target "$HOST_TARGET" >"$build_log" 2>&1; then
	build_elapsed=$((SECONDS - build_start))
	printf 'FAILED after %ds — see %s\n' "$build_elapsed" "$build_log" >&2
	tail -30 "$build_log" >&2
	exit 1
fi
build_elapsed=$((SECONDS - build_start))
printf 'done in %ds.\n\n' "$build_elapsed"

failures=0
for target in $targets; do
	log="$LOG_DIR/$target.log"
	printf '%-32s ' "$target"
	if cargo fuzz run --target "$HOST_TARGET" "$target" \
			-- -max_total_time="$FUZZ_TIME" \
			   -verbosity=0 \
			   -print_final_stats=1 \
			>"$log" 2>&1; then
		execs=$(awk '/stat::number_of_executed_units/ {print $NF}' "$log" | tail -1)
		rate=$(awk '/stat::average_exec_per_sec/ {print $NF}' "$log" | tail -1)
		printf 'OK    (%s execs, %s/s)\n' "${execs:-?}" "${rate:-?}"
	else
		printf 'CRASH — see fuzz/logs/%s.log\n' "$target"
		# Surface the actionable bits: panic message + reproducer path.
		grep -E 'panicked at|Test unit written to|^SUMMARY: ' "$log" \
			| sed 's/^/    /' || true
		failures=$((failures + 1))
	fi
done

echo
if [ "$failures" -gt 0 ]; then
	cat <<EOF
$failures target(s) crashed.

Reproduce:
  cd fuzz
  cargo fuzz run <target> artifacts/<target>/<crash-file>

Minimise the input:
  cargo fuzz tmin <target> artifacts/<target>/<crash-file>

Pretty-print the bytes (Debug formatter):
  cargo fuzz fmt <target> artifacts/<target>/<crash-file>
EOF
	exit 1
else
	echo "All ${FUZZ_TIME}s runs clean."
fi
