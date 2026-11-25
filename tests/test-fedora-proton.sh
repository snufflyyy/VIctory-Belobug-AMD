#!/bin/sh
# Checks the Fedora Proton launcher's VICTORY_CONSOLE handling,
# VICTORY_SKIP_MOVIES passthrough, VICTORY_HAIR default, and the
# VICTORY_AMBIENT default. bwrap is replaced by a stub that only records
# its arguments, so no GPU, Proton prefix, or game file is touched.
# Run: sh tests/test-fedora-proton.sh
#
# Adapted from the glm-console branch's tests/test-fedora-proton.sh
# (commit 12b42e7) to the launcher as integrated on fedora-proton: this
# launcher prints only the state line, so the expected output checks the
# recorded bwrap arguments, not a console-target line.
set -eu

repo=$(realpath "$(dirname "$0")/..")
launcher=$repo/scripts/fedora-proton.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# A fake desktop session and a fake Steam/Proton tree.
mkdir -p "$tmp/steam" "$tmp/proton/files/bin" "$tmp/bin" "$tmp/state" "$tmp/runtime"
printf '#!/bin/sh\nprintf "%%s\\n" "$@" >"$BWRAP_ARGS"\n' >"$tmp/bin/bwrap"
printf '#!/bin/sh\n' >"$tmp/proton/files/bin/wine"
chmod +x "$tmp/bin/bwrap" "$tmp/proton/files/bin/wine"
touch "$tmp/proton/proton"
printf 'stub' >"$tmp/victory.exe"
printf 'stub' >"$tmp/title.exe"

PATH="$tmp/bin:$PATH"
export PATH
export STEAM_ROOT="$tmp/steam" PROTON_DIR="$tmp/proton"
export VICTORY_PROTON_STATE="$tmp/state"
export DISPLAY=:99 XDG_RUNTIME_DIR="$tmp/runtime"
export BWRAP_ARGS="$tmp/bwrap.args"
export MANGOHUD=0

fail_test() { printf '%s\n' "$*" >&2; exit 1; }

# The value the launcher handed the executable through bwrap's
# environment for VICTORY_SKIP_MOVIES.
skip_movies_env() {
    awk '/^VICTORY_SKIP_MOVIES$/ { getline; print; exit }' "$tmp/bwrap.args"
}

# The value the launcher set VICTORY_HAIR to inside the sandbox, if any.
hair_env() {
    awk '/^--setenv$/ { getline; if ($0 == "VICTORY_HAIR") { getline; print; exit } }' "$tmp/bwrap.args"
}

# The value the launcher set VICTORY_AMBIENT to inside the sandbox.
ambient_env() {
    awk '/^--setenv$/ { getline; if ($0 == "VICTORY_AMBIENT") { getline; print; exit } }' "$tmp/bwrap.args"
}

expect_console() {
    expected=$1
    sh "$launcher" --check "$tmp/victory.exe" "$tmp/title.exe" >"$tmp/out" 2>&1 ||
        fail_test "launcher exited nonzero with VICTORY_CONSOLE=${VICTORY_CONSOLE:-unset}"
    grep -qx "VIctory state: $tmp/state" "$tmp/out" ||
        fail_test 'launcher output lacks the state line'
    [ "$(tail -n 1 "$tmp/bwrap.args")" = "$expected" ] ||
        fail_test "bwrap last argument is not '$expected'"
    [ "$(tail -n 4 "$tmp/bwrap.args" | head -n 1)" = victory-proton ] ||
        fail_test "console was not passed as the third inner-shell argument"
    [ "$(tail -n 2 "$tmp/bwrap.args" | head -n 1)" = "$(basename "$tmp/title.exe")" ] ||
        fail_test 'the title name is not the second inner-shell argument'
}

sh -n "$launcher" || fail_test 'launcher fails sh -n'

# The Series S profile is the default; Series X stays available for
# comparison runs.
unset VICTORY_CONSOLE
expect_console series-s

VICTORY_CONSOLE=series-x; export VICTORY_CONSOLE
expect_console series-x

VICTORY_CONSOLE=series-s; export VICTORY_CONSOLE
expect_console series-s

VICTORY_CONSOLE=series-sx; export VICTORY_CONSOLE
if sh "$launcher" --check "$tmp/victory.exe" "$tmp/title.exe" >"$tmp/out" 2>&1; then
    fail_test 'invalid VICTORY_CONSOLE was accepted'
fi
grep -q "VICTORY_CONSOLE must be series-x or series-s, not 'series-sx'." "$tmp/out" ||
    fail_test 'invalid VICTORY_CONSOLE produced the wrong diagnostic'
unset VICTORY_CONSOLE

# VICTORY_SKIP_MOVIES passes through to the sandbox unset when unset,
# and with its value when it has one.
expect_console series-s
[ "$(skip_movies_env)" = '' ] ||
    fail_test 'VICTORY_SKIP_MOVIES unset did not pass through empty'

VICTORY_SKIP_MOVIES=1; export VICTORY_SKIP_MOVIES
expect_console series-s
[ "$(skip_movies_env)" = 1 ] ||
    fail_test 'VICTORY_SKIP_MOVIES=1 did not pass through'

VICTORY_SKIP_MOVIES=all; export VICTORY_SKIP_MOVIES
expect_console series-s
[ "$(skip_movies_env)" = all ] ||
    fail_test 'VICTORY_SKIP_MOVIES=all did not pass through'
unset VICTORY_SKIP_MOVIES

# VICTORY_HAIR is forwarded as 1 unless the exact value 0 opts out,
# because the executable reads any set value, even 0, as opt-in.
unset VICTORY_HAIR
expect_console series-s
[ "$(hair_env)" = 1 ] ||
    fail_test 'VICTORY_HAIR unset was not forwarded as the hair-on default'

VICTORY_HAIR=; export VICTORY_HAIR
expect_console series-s
[ "$(hair_env)" = 1 ] ||
    fail_test 'empty VICTORY_HAIR was not forwarded as the hair-on default'

VICTORY_HAIR=1; export VICTORY_HAIR
expect_console series-s
[ "$(hair_env)" = 1 ] ||
    fail_test 'VICTORY_HAIR=1 did not pass through as --setenv VICTORY_HAIR 1'

VICTORY_HAIR=0; export VICTORY_HAIR
expect_console series-s
if grep -qx 'VICTORY_HAIR' "$tmp/bwrap.args"; then
    fail_test 'VICTORY_HAIR=0 was still passed to the sandbox'
fi

VICTORY_HAIR=on; export VICTORY_HAIR
if sh "$launcher" --check "$tmp/victory.exe" "$tmp/title.exe" >"$tmp/out" 2>&1; then
    fail_test 'invalid VICTORY_HAIR was accepted'
fi
grep -q "VICTORY_HAIR must be 0, 1, or unset, not 'on'." "$tmp/out" ||
    fail_test 'invalid VICTORY_HAIR produced the wrong diagnostic'
unset VICTORY_HAIR

# VICTORY_AMBIENT defaults to off, keeping the title's own resolve output;
# an operator's value passes through as set.
unset VICTORY_AMBIENT
expect_console series-s
[ "$(ambient_env)" = off ] ||
    fail_test 'VICTORY_AMBIENT unset did not default to off'

VICTORY_AMBIENT=0.6; export VICTORY_AMBIENT
expect_console series-s
[ "$(ambient_env)" = 0.6 ] ||
    fail_test 'VICTORY_AMBIENT=0.6 did not pass through'

VICTORY_AMBIENT=off; export VICTORY_AMBIENT
expect_console series-s
[ "$(ambient_env)" = off ] ||
    fail_test 'VICTORY_AMBIENT=off did not pass through'
unset VICTORY_AMBIENT

printf '%s\n' 'fedora-proton launcher console tests passed'
