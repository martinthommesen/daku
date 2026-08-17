#!/bin/sh
# macOS .app packager. Release embeds Sparkle + daku-daemon and writes a DMG.
set -eu

usage() {
  cat <<'EOF'
usage: scripts/bundle.sh [debug|release] [--unsigned]
       scripts/bundle.sh --unsigned
       scripts/bundle.sh --help

  debug       local Debug.app under target/debug (default)
  release     Daku.app + daku-daemon + Sparkle + DMG
  --unsigned  skip codesign; write dist/Daku.app
              (release implied when no profile is given)

Unsigned local bundle:
  ./scripts/bundle.sh --unsigned

Homebrew-channel DMG (Sparkle compiled out):
  DAKU_CHANNEL=homebrew ./scripts/bundle.sh --unsigned
EOF
}

profile=""
unsigned=0
if [ "${SKIP_CODESIGN:-0}" = "1" ]; then
  unsigned=1
fi

for arg in "$@"; do
  case "$arg" in
    --help|-h)
      usage
      exit 0
      ;;
    --unsigned)
      unsigned=1
      ;;
    debug|release)
      profile="$arg"
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

if [ -z "$profile" ]; then
  if [ "$unsigned" = "1" ]; then
    profile="release"
  else
    profile="debug"
  fi
fi

cargo_target_dir="${CARGO_TARGET_DIR:-target}"
debug_identity_cache=".daku-cache/codesign/debug-identity"
codesign_identity_from_environment=0
if [ "$unsigned" = "1" ]; then
  codesign_identity="-"
elif [ -n "${DAKU_CODESIGN_IDENTITY:-}" ]; then
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
    app_name="Daku"
    bundle_identifier="app.daku"
    icon_file="AppIcon.icns"
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

if [ "$profile" = "debug" ] && [ "$unsigned" = "0" ] && [ "$codesign_identity_from_environment" = "0" ] && [ "$codesign_identity" != "-" ]; then
  mkdir -p "$(dirname "$debug_identity_cache")"
  printf '%s\n' "$codesign_identity" > "$debug_identity_cache"
fi
debug_adhoc_requirement="=designated => identifier \"$bundle_identifier\""
homebrew_channel=0
if [ "${DAKU_CHANNEL:-}" = "homebrew" ]; then
  homebrew_channel=1
fi
if [ "${DAKU_SKIP_CARGO_BUILD:-0}" != "1" ]; then
  if [ "$profile" = "release" ] && [ "$homebrew_channel" = "1" ]; then
    cargo build --release --features channel-homebrew --package daku --bin daku --package daku-daemon --bin daku-daemon
  elif [ "$profile" = "release" ]; then
    cargo build --release --package daku --bin daku --package daku-daemon --bin daku-daemon
  else
    cargo build --package daku --bin daku
  fi
fi

bundle="$cargo_target_dir/$profile/$app_name.app"
contents="$bundle/Contents"
daemon_executable="$contents/MacOS/daku-daemon"
version=$(awk -F\" '/^version = / { print $2; exit }' Cargo.toml)

# Sparkle 2.9.4 — bump version and checksum together. Cached outside target/
# so `cargo clean` does not evict the download.
sparkle_version="2.9.4"
sparkle_sha256="ce89daf967db1e1893ed3ebd67575ed82d3902563e3191ca92aaec9164fbdef9"
sparkle_cache_root=".daku-cache/sparkle"
sparkle_cache_entry="$sparkle_cache_root/$sparkle_version"
sparkle_framework_source="$sparkle_cache_entry/Sparkle.framework"
sparkle_ok=0

ensure_sparkle() {
  if [ -d "$sparkle_framework_source" ]; then
    sparkle_ok=1
    return 0
  fi
  sparkle_staging="$sparkle_cache_root/.staging-$sparkle_version-$$"
  rm -rf "$sparkle_staging"
  mkdir -p "$sparkle_staging"
  sparkle_archive="$sparkle_staging/Sparkle-$sparkle_version.tar.xz"
  if ! curl -fsSL --retry 3 -o "$sparkle_archive" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$sparkle_version/Sparkle-$sparkle_version.tar.xz"; then
    rm -rf "$sparkle_staging"
    return 1
  fi
  if ! echo "$sparkle_sha256  $sparkle_archive" | shasum -a 256 -c - >/dev/null; then
    echo "error: Sparkle $sparkle_version archive checksum mismatch" >&2
    rm -rf "$sparkle_staging"
    return 1
  fi
  if ! tar -xJf "$sparkle_archive" -C "$sparkle_staging" ./Sparkle.framework ./bin; then
    rm -rf "$sparkle_staging"
    return 1
  fi
  rm "$sparkle_archive"
  mv "$sparkle_staging" "$sparkle_cache_entry" || return 1
  sparkle_ok=1
}

if [ "$profile" = "release" ] && [ "$homebrew_channel" = "0" ]; then
  if ! ensure_sparkle; then
    if [ "$unsigned" = "1" ]; then
      echo "warning: Sparkle download failed; unsigned bundle will omit the framework" >&2
    else
      echo "error: Sparkle $sparkle_version is required for a signed release" >&2
      exit 1
    fi
  fi
fi

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
plutil -replace CFBundleShortVersionString -string "$version" "$contents/Info.plist"
# Sparkle compares CFBundleVersion (sparkle:version). Keep it equal to the
# semver so each release is strictly newer than the last.
plutil -replace CFBundleVersion -string "$version" "$contents/Info.plist"

sparkle_framework=""
if [ "$profile" = "release" ] && [ "$sparkle_ok" = "1" ]; then
  frameworks_directory="$contents/Frameworks"
  sparkle_framework="$frameworks_directory/Sparkle.framework"
  mkdir -p "$frameworks_directory"
  # ditto keeps Sparkle's relative version symlinks intact.
  ditto "$sparkle_framework_source" "$sparkle_framework"
  for sparkle_extra in XPCServices Headers PrivateHeaders Modules; do
    rm -rf "$sparkle_framework/$sparkle_extra" \
      "$sparkle_framework/Versions/B/$sparkle_extra"
  done
fi

xattr -cr "$bundle"

if [ "$unsigned" = "1" ]; then
  :
elif [ "$codesign_identity" = "-" ]; then
  if [ -n "$sparkle_framework" ]; then
    codesign --force --sign - "$sparkle_framework/Versions/B/Autoupdate"
    codesign --force --sign - "$sparkle_framework/Versions/B/Updater.app"
    codesign --force --sign - "$sparkle_framework"
  fi
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
  if [ -n "$sparkle_framework" ]; then
    codesign --force --options runtime --timestamp --sign "$codesign_identity" "$sparkle_framework/Versions/B/Autoupdate"
    codesign --force --options runtime --timestamp --sign "$codesign_identity" "$sparkle_framework/Versions/B/Updater.app"
    codesign --force --options runtime --timestamp --sign "$codesign_identity" "$sparkle_framework"
  fi
  codesign --force --options runtime --timestamp --identifier "$bundle_identifier.daemon" --sign "$codesign_identity" "$daemon_executable"
  codesign --force --options runtime --timestamp --sign "$codesign_identity" "$bundle"
else
  codesign --force --options runtime --sign "$codesign_identity" "$bundle"
fi

if [ "$profile" = "release" ] && [ "$unsigned" = "0" ]; then
  codesign --verify --strict --verbose=2 "$daemon_executable"
  codesign --verify --deep --strict --verbose=2 "$bundle"
fi

if [ "$profile" = "release" ]; then
  mkdir -p dist
  rm -rf dist/Daku.app
  ditto "$bundle" dist/Daku.app
  dmg_staging="dist/.dmg-staging-$$"
  rm -rf "$dmg_staging"
  mkdir -p "$dmg_staging"
  ditto dist/Daku.app "$dmg_staging/Daku.app"
  ln -s /Applications "$dmg_staging/Applications"
  if [ "$homebrew_channel" = "1" ]; then
    dmg_path="dist/Daku-$version-homebrew.dmg"
  else
    dmg_path="dist/Daku-$version.dmg"
  fi
  rm -f "$dmg_path"
  hdiutil create -volname Daku -srcfolder "$dmg_staging" -ov -format UDZO "$dmg_path" >/dev/null
  rm -rf "$dmg_staging"
  echo "$dmg_path"
fi

echo "$bundle"
if [ "$profile" = "release" ]; then
  echo "dist/Daku.app"
fi
