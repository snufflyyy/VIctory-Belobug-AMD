#!/bin/sh
# Runs the Windows build under Steam Proton on Fedora without network access.
set -eu

fail() { printf '%s\n' "$*" >&2; exit 1; }
mode=--execute
if [ "${1:-}" = --check ]; then
    mode=--check
    shift
fi
[ "$#" -eq 2 ] || fail 'Usage: sh scripts/fedora-proton.sh [--check] /path/to/victory.exe /path/to/title.exe'
console=${VICTORY_CONSOLE:-series-x}
case "$console" in
    series-x | series-s) ;;
    *) fail "VICTORY_CONSOLE must be series-x or series-s, not '$console'." ;;
esac
command -v bwrap >/dev/null || fail 'Install bubblewrap first.'
[ -n "${DISPLAY:-}" ] || fail 'Run from a desktop session with XWayland or X11.'

repo=$(realpath "$(dirname "$0")/..")
victory=$(realpath -e -- "$1")
title=$(realpath -e -- "$2")
[ -f "$victory" ] && [ -f "$title" ] || fail 'Both executable paths must name files.'
state=$(realpath -m -- "${VICTORY_PROTON_STATE:-${XDG_STATE_HOME:-$HOME/.local/state}/victory-proton}")
for path in "$title" "$state"; do
    case "$path/" in "$repo/"*) fail 'Keep the title and Proton state outside the checkout.' ;; esac
done
steam=$(realpath -e -- "${STEAM_ROOT:-$HOME/.local/share/Steam}")
proton=$(realpath -e -- "${PROTON_DIR:-$steam/steamapps/common/Proton - Experimental}")
[ -x "$proton/files/bin/wine" ] && [ -f "$proton/proton" ] || fail 'Install Proton Experimental in Steam or set PROTON_DIR.'
if [ "${MANGOHUD:-1}" = 1 ] && ! command -v mangohud >/dev/null; then
    printf '%s\n' 'MangoHud is not installed. Install mangohud to see the overlay.' >&2
fi

# Keep saves and caches private to this launcher and prevent concurrent use.
umask 077
mkdir -p "$state/home" "$state/compat" "$state/vkd3d" "$state/cache" "$state/data"
exec 9>"$state/launcher.lock"
flock -n 9 || fail "This Proton prefix is already in use: $state"
touch "$state/dist.lock"
printf 'VIctory state: %s\n' "$state"

# Copy-based GPU memory is the working path on Fedora/RADV. Do not import
# foreign DLLs automatically; use a compiler the operator supplied.
overrides='d3d12,d3d12core,dxgi=n,b'
set --
if [ -n "${VICTORY_WINESERVER:-}" ]; then
    wineserver=$(realpath -e -- "$VICTORY_WINESERVER")
    [ -f "$wineserver" ] && [ -x "$wineserver" ] || fail 'VICTORY_WINESERVER must name an executable built for this Proton version.'
    # Wine prefers the server in its own directory over the WINESERVER variable.
    # Mount over that path only in this sandbox; never replace Steam files.
    set -- "$@" --ro-bind "$wineserver" /proton/files/bin/wineserver
fi
if [ -n "${VICTORY_NATIVE_FXC:-}" ]; then
    fxc=$(realpath -e -- "$VICTORY_NATIVE_FXC")
    [ -f "$fxc" ] || fail 'VICTORY_NATIVE_FXC must name a d3dcompiler_47.dll file.'
    overrides="$overrides;d3dcompiler_47=n,b"
    set -- "$@" --ro-bind "$fxc" /app/d3dcompiler_47.dll
fi
if [ -n "${VICTORY_SHADER_BUILD_OF:-}" ]; then
    prior=$(realpath -e -- "$VICTORY_SHADER_BUILD_OF")
    [ -f "$prior" ] || fail 'VICTORY_SHADER_BUILD_OF must name a previous executable with the same translator.'
    set -- "$@" --ro-bind "$prior" /app/previous-build.exe \
        --setenv VICTORY_SHADER_BUILD_OF 'Z:\app\previous-build.exe'
fi
if [ -n "${VICTORY_CAPTURE_FRAMES:-}" ]; then
    mkdir -p "$state/frames"
    set -- "$@" --setenv VICTORY_CAPTURE 'Z:\session\frames' \
        --setenv VICTORY_CAPTURE_FRAMES "$VICTORY_CAPTURE_FRAMES"
fi
if [ -n "${XAUTHORITY:-}" ] && [ -f "$XAUTHORITY" ]; then
    set -- "$@" --ro-bind "$XAUTHORITY" /run/game-xauthority --setenv XAUTHORITY /run/game-xauthority
fi
pulse=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/pulse/native
if [ -S "$pulse" ]; then
    set -- "$@" --ro-bind "$pulse" /run/pulse/native --setenv PULSE_SERVER unix:/run/pulse/native
fi

bwrap --die-with-parent --unshare-pid --unshare-net --unshare-ipc --unshare-uts \
    --clearenv --proc /proc --dev /dev --dev-bind /dev/dri /dev/dri \
    --dev-bind-try /dev/ntsync /dev/ntsync \
    --ro-bind /usr /usr --ro-bind /etc /etc --ro-bind /sys /sys \
    --symlink usr/bin /bin --symlink usr/lib /lib --symlink usr/lib64 /lib64 \
    --tmpfs /tmp --ro-bind /tmp/.X11-unix /tmp/.X11-unix \
    --dir /steam --ro-bind-try "$steam/legacycompat" /steam/legacycompat \
    --ro-bind-try "$steam/steamclient.dll" /steam/steamclient.dll \
    --ro-bind-try "$steam/steamclient64.dll" /steam/steamclient64.dll \
    --ro-bind-try "$steam/ubuntu12_64/video" /steam/ubuntu12_64/video \
    --ro-bind "$proton" /proton --bind "$state/dist.lock" /proton/dist.lock \
    --ro-bind "$(dirname "$title")" /game --ro-bind "$victory" /app/victory.exe \
    "$@" --bind "$state" /session --chdir /session \
    --setenv PATH /usr/bin:/bin --setenv LANG C.UTF-8 --setenv HOME /session/home \
    --setenv USER "$(id -un)" --setenv DISPLAY "$DISPLAY" \
    --setenv DBUS_SESSION_BUS_ADDRESS unix:path=/session/no-session-bus --setenv NO_AT_BRIDGE 1 \
    --setenv STEAM_COMPAT_DATA_PATH /session/compat --setenv STEAM_COMPAT_CLIENT_INSTALL_PATH /steam \
    --setenv SteamAppId 0 --setenv SteamGameId 0 \
    --setenv WINEDLLOVERRIDES "$overrides" --setenv WINEDEBUG "${WINEDEBUG:--all,err+all}" \
    --setenv VKD3D_SHADER_CACHE_PATH /session/vkd3d --setenv VICTORY_COPY_MEMORY 1 \
    --setenv VICTORY_WINDOW_MAX "${VICTORY_WINDOW_MAX:-1280x720}" \
    --setenv VICTORY_NO_WATCHDOG "${VICTORY_NO_WATCHDOG:-0}" \
    --setenv VICTORY_KEYS "${VICTORY_KEYS:-}" \
    --setenv VICTORY_SKIP_MOVIES "${VICTORY_SKIP_MOVIES:-}" \
    --setenv MANGOHUD "${MANGOHUD:-1}" \
    --setenv MANGOHUD_CONFIG "${MANGOHUD_CONFIG:-fps,frame_timing,gpu_stats,cpu_stats,ram,vram,resolution}" \
    --setenv VICTORY_GAME_DIR 'Z:\game' --setenv VICTORY_CACHE_DIR 'Z:\session\cache' \
    --setenv VICTORY_DATA_DIR 'Z:\session\data' --setenv VICTORY_LOG "${VICTORY_LOG:-info}" \
    -- /bin/sh -c '
        set -eu
        if [ ! -s /session/compat/pfx/system.reg ]; then
            # The Steam stub can return nonzero even after Proton installs the prefix.
            timeout --kill-after=5s 45s python3 /proton/proton run "C:\windows\system32\cmd.exe" /c exit > /session/prefix.log 2>&1 || true
            if [ ! -s /session/compat/pfx/system.reg ]; then
                tail -40 /session/prefix.log >&2
                exit 1
            fi
        fi
        mode=$1
        title=$2
        console=$3
        set -- run "Z:\game\\${title}" --console "$console"
        if [ "$mode" = --execute ]; then set -- "$@" --execute; fi
        exec python3 /proton/proton runinprefix "Z:\app\victory.exe" "$@"
    ' victory-proton "$mode" "$(basename "$title")" "$console"
