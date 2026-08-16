#!/usr/bin/env bash
# Builds the two Core Audio HAL plug-ins that carry audio between apps and the
# wsctl audio bridge. Needs the Command Line Tools; Xcode is not required.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/vendor/BlackHole.c"
OUT="${1:-$HERE/build}"
VERSION=0.7.1

# Identifies each plug-in to Core Audio. Stable by design: regenerating these
# changes the device UIDs, and anything that had selected a device loses it.
UUID_SPEAKER=A471EEC2-0DC7-4AAC-8B24-638687205A4B
UUID_MIC=C10543A4-BC34-47CD-B4DC-18F612C42E2A

build_bundle() {
  local product=$1 bundle_id=$2 uuid=$3
  shift 3
  local drv="$OUT/$product.driver"

  rm -rf "$drv"
  mkdir -p "$drv/Contents/MacOS"

  clang -bundle -o "$drv/Contents/MacOS/$product" "$SRC" \
    -arch arm64 -arch x86_64 -mmacosx-version-min=13.0 -O2 \
    -framework CoreAudio -framework CoreFoundation -framework Accelerate \
    -DkPlugIn_BundleID="\"$bundle_id\"" \
    -DkHas_Driver_Name_Format=false \
    -DkNumber_Of_Channels=2 \
    -DkManufacturer_Name='"Workstation"' \
    "$@"

  sed -e "s|@PRODUCT@|$product|g" \
      -e "s|@BUNDLE_ID@|$bundle_id|g" \
      -e "s|@FACTORY_UUID@|$uuid|g" \
      -e "s|@VERSION@|$VERSION|g" \
      "$HERE/Info.plist.in" > "$drv/Contents/Info.plist"

  # Sign last. The signature seals Info.plist, and the loader hard-rejects an
  # invalid signature while merely tolerating an absent one, so editing the
  # plist after signing silently stops the plug-in loading.
  codesign --force --sign - "$drv"
  codesign --verify "$drv"
  echo "  built $product.driver"
}

rm -rf "$OUT"
mkdir -p "$OUT"

# Apps play into "Workstation Speaker"; the bridge reads the same audio back out of
# "Workstation Speaker Tap". Two devices in one bundle share a ring buffer, which is
# exactly the one pipe this direction needs.
build_bundle WSSpeaker dev.pj.workstation.WSSpeaker "$UUID_SPEAKER" \
  -DkDriver_Name='"WSSpeaker"' \
  -DkDevice_Name='"Workstation Speaker"'      -DkDevice_IsHidden=false  -DkDevice_HasInput=false -DkDevice_HasOutput=true \
  -DkDevice2_Name='"Workstation Speaker Tap"' -DkDevice2_IsHidden=false -DkDevice2_HasInput=true -DkDevice2_HasOutput=false

# The bridge writes cleaned audio into "Workstation Mic Feed"; apps read it
# from "Workstation Mic". Separate bundle so it is an independent pipe.
build_bundle WSMicrophone dev.pj.workstation.WSMicrophone "$UUID_MIC" \
  -DkDriver_Name='"WSMicrophone"' \
  -DkDevice_Name='"Workstation Mic"'       -DkDevice_IsHidden=false -DkDevice_HasInput=true  -DkDevice_HasOutput=false \
  -DkDevice2_Name='"Workstation Mic Feed"' -DkDevice2_IsHidden=false -DkDevice2_HasInput=false -DkDevice2_HasOutput=true

echo "output in $OUT"
