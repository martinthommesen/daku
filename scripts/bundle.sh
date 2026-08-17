#!/bin/sh
# macOS .app packager for local debug / release. Sparkle + DMG live in plan 010.
set -eu

profile="${1:-debug}"
cargo_target_dir="${CARGO_TARGET_DIR:-target}"
debug_identity_cache=".daku-cache/codesign/debug-identity"
codesign_identity_from_environment=0
if [ -n "${DAKU_CODESIGN_IDENTITY:-}" ]; then
  codesign_identity="$DAKU_CODESIGN_IDENTITY"
  codesign_identity_from_environment=1
else
  if [ "$profile" = "debug" ]; then
    preferred_identity="Apple Development:"
    fallback_identity="Developer ID Application:"
  else
    preferred_identity="Developer ID Application:"
    fallback_identity="Apple Development:"
  fi
  codesign_identity=""
  if [ "$profile" = "debug" ] && [ -f "$debug_identity_cache" ]; then
    IFS= read -r cached_identity < "$debug_identity_cache" || cached_identity=""
    if [ -n "$cached_identity" ]; then
      codesign_identity=$(security find-identity -v -p codesigning 2>/dev/null \
        | awk -v identity="$cached_identity" 'index($0, identity) { print $2; exit }')
    fi
  fi
  if [ -z "$codesign_identity" ]; then
    codesign_identity=$(security find-identity -v -p codesigning 2>/dev/null \
      | awk -v identity="$preferred_identity" 'index($0, "\"" identity) { print $2; exit }')
  fi
  if [ -z "$codesign_identity" ]; then
    codesign_identity=$(security find-identity -v -p codesigning 2>/dev/null \
      | awk -v identity="$fallback_identity" 'index($0, "\"" identity) { print $2; exit }')
  fi
  if [ -z "$codesign_identity" ]; then
    codesign_identity="-"
  fi
fi
case "$profile" in
  debug)
    app_name="daku Debug"
    bundle_identifier="app.daku.dev"
    icon_file="AppIconDev.icns"
    ;;
  release)
    app_name="daku"
    bundle_identifier="app.daku"
    icon_file="AppIcon.icns"
    ;;
  *)
    echo "usage: scripts/bundle.sh [debug|release]" >&2
    exit 2
    ;;
esac
if [ "$profile" = "debug" ] && [ "$codesign_identity_from_environment" = "0" ] && [ "$codesign_identity" != "-" ]; then
  mkdir -p "$(dirname "$debug_identity_cache")"
  printf '%s\n' "$codesign_identity" > "$debug_identity_cache"
fi
debug_adhoc_requirement="=designated => identifier \"$bundle_identifier\""
if [ "${DAKU_SKIP_CARGO_BUILD:-0}" != "1" ]; then
  if [ "$profile" = "release" ]; then
    cargo build --release --package daku --bin daku --package daku-daemon --bin daku-daemon
  else
    cargo build --package daku --bin daku
  fi
fi

bundle="$cargo_target_dir/$profile/$app_name.app"
contents="$bundle/Contents"
daemon_executable="$contents/MacOS/daku-daemon"

rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp "$cargo_target_dir/$profile/daku" "$contents/MacOS/$app_name"
chmod 755 "$contents/MacOS/$app_name"
if [ "$profile" = "release" ]; then
  cp "$cargo_target_dir/$profile/daku-daemon" "$daemon_executable"
  chmod 755 "$daemon_executable"
fi
cp resources/Info.plist "$contents/Info.plist"
cp "resources/$icon_file" "$contents/Resources/AppIcon.icns"
plutil -replace CFBundleDisplayName -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleExecutable -string "$app_name" "$contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$bundle_identifier" "$contents/Info.plist"
plutil -replace CFBundleName -string "$app_name" "$contents/Info.plist"
# Finder info and resource forks on copied resources make codesign reject the
# bundle as "detritus"; strip extended attributes before signing.
xattr -cr "$bundle"
if [ "$codesign_identity" = "-" ]; then
  if [ "$profile" = "release" ]; then
    codesign --force --identifier "$bundle_identifier.daemon" --sign - "$daemon_executable"
    codesign --force --sign - "$bundle"
  else
    # An ordinary ad-hoc signature's designated requirement contains its
    # changing code hash, so macOS TCC treats every rebuild as a different app
    # and repeatedly asks for Files & Folders access. The development-only
    # bundle id is a stable local identity even when no trusted Apple
    # Development certificate is installed.
    codesign --force --identifier "$bundle_identifier" --requirements "$debug_adhoc_requirement" --sign - "$bundle"
  fi
elif [ "$profile" = "release" ]; then
  codesign --force --options runtime --timestamp --identifier "$bundle_identifier.daemon" --sign "$codesign_identity" "$daemon_executable"
  codesign --force --options runtime --timestamp --sign "$codesign_identity" "$bundle"
else
  codesign --force --options runtime --sign "$codesign_identity" "$bundle"
fi
if [ "$profile" = "release" ]; then
  codesign --verify --strict --verbose=2 "$daemon_executable"
  codesign --verify --deep --strict --verbose=2 "$bundle"
fi

echo "$bundle"
