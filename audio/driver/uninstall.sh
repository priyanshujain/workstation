#!/usr/bin/env bash
# Removes the wsctl HAL plug-ins. Run with sudo.
set -euo pipefail

HAL=/Library/Audio/Plug-Ins/HAL

if [[ $EUID -ne 0 ]]; then
  echo "needs root: sudo $0" >&2
  exit 1
fi

for d in WSSpeaker WSMicrophone; do
  if [[ -d "$HAL/$d.driver" ]]; then
    rm -rf "$HAL/$d.driver"
    echo "  removed $d.driver"
  fi
done

killall coreaudiod 2>/dev/null || true
echo "done. audio cuts out for a second while coreaudiod restarts."
