#!/usr/bin/env bash
# Installs the wsctl HAL plug-ins into /Library/Audio/Plug-Ins/HAL. Run with sudo.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD="${1:-$HERE/build}"
HAL=/Library/Audio/Plug-Ins/HAL

if [[ $EUID -ne 0 ]]; then
  echo "needs root: sudo $0" >&2
  exit 1
fi

for d in WSSpeaker WSMicrophone; do
  src="$BUILD/$d.driver"
  [[ -d "$src" ]] || { echo "missing $src, run build.sh first" >&2; exit 1; }
  # Replace rather than update in place: a partially overwritten bundle fails
  # its signature check, and the loader rejects an invalid signature outright.
  rm -rf "$HAL/$d.driver"
  cp -R "$src" "$HAL/"
  chown -R root:wheel "$HAL/$d.driver"
  find "$HAL/$d.driver" -type d -exec chmod 755 {} +
  find "$HAL/$d.driver" -type f -exec chmod 644 {} +
  chmod 755 "$HAL/$d.driver/Contents/MacOS/$d"
  echo "  installed $d.driver"
done

# Plug-ins are only scanned at startup; launchd brings coreaudiod straight back.
killall coreaudiod 2>/dev/null || true
echo "done. audio cuts out for a second while coreaudiod restarts."
