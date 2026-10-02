#!/bin/bash
set -euo pipefail

usage() {
    printf 'Usage: bash tools/build-macos.sh [--expected-tag v<workspace-version>] [--require-signing] [--require-update-key] [--output DIR]\n'
}

expected_tag=''
require_signing=false
require_update_key=false
output=''
while (($#)); do
    case "$1" in
        --expected-tag)
            if (($# < 2)) || [[ -z "$2" ]]; then
                printf '%s\n' '--expected-tag requires a value' >&2
                exit 2
            fi
            expected_tag=$2
            shift 2
            ;;
        --output)
            if (($# < 2)) || [[ -z "$2" ]]; then
                printf '%s\n' '--output requires a directory' >&2
                exit 2
            fi
            output=$2
            shift 2
            ;;
        --require-signing) require_signing=true; shift ;;
        --require-update-key) require_update_key=true; shift ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
version=''
in_package=false
while IFS= read -r line || [[ -n "$line" ]]; do
    line=${line%$'\r'}
    if [[ "$line" =~ ^\[workspace\.package\][[:space:]]*$ ]]; then
        in_package=true
    elif [[ "$line" =~ ^\[ ]]; then
        in_package=false
    elif $in_package && [[ "$line" =~ ^version[[:space:]]*=[[:space:]]*\"([^\"]+)\" ]]; then
        version=${BASH_REMATCH[1]}
        break
    fi
done < "$project_root/Cargo.toml"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
    printf 'Could not read a valid Compi workspace version from Cargo.toml\n' >&2
    exit 1
fi
if [[ -n "$expected_tag" && "$expected_tag" != "v$version" ]]; then
    printf 'Release tag %s does not match Cargo workspace version %s\n' "$expected_tag" "$version" >&2
    exit 1
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    printf 'Build on a native ARM64 macOS host (the release runner is macos-14).\n' >&2
    exit 1
fi

signing_identity=${COMPI_MACOS_SIGNING_IDENTITY:-}
notary_profile=${COMPI_MACOS_NOTARY_PROFILE:-}
notary_arguments=(--keychain-profile "$notary_profile")
if [[ -n "${COMPI_MACOS_NOTARY_KEYCHAIN:-}" ]]; then
    notary_arguments+=(--keychain "$COMPI_MACOS_NOTARY_KEYCHAIN")
fi
if [[ -n "$signing_identity" && -z "$notary_profile" ]] || [[ -z "$signing_identity" && -n "$notary_profile" ]]; then
    printf 'Configure both COMPI_MACOS_SIGNING_IDENTITY and COMPI_MACOS_NOTARY_PROFILE, or neither.\n' >&2
    exit 1
fi
if $require_signing && [[ -z "$signing_identity" ]]; then
    printf 'This signed build requires Developer ID signing and a notarytool keychain profile.\n' >&2
    exit 1
fi
if $require_update_key && [[ -z "${COMPI_UPDATE_PUBLIC_KEY:-}" ]]; then
    printf 'Trusted public updates require COMPI_UPDATE_PUBLIC_KEY at compile time.\n' >&2
    exit 1
fi
if [[ -z "${COMPI_UPDATE_PUBLIC_KEY:-}" ]]; then
    printf 'Dogfood build: public updater disabled (no verification key configured).\n' >&2
fi
if [[ -n "${COMPI_UPDATE_PUBLIC_KEY:-}" ]]; then
    python3 - <<'PYTHON'
import base64
import os
key = base64.b64decode(os.environ["COMPI_UPDATE_PUBLIC_KEY"], validate=True)
if len(key) != 32:
    raise SystemExit("COMPI_UPDATE_PUBLIC_KEY must encode a 32-byte Ed25519 public key")
PYTHON
fi

# GPUI 0.2.2 build.rs targets 10.15.7 for Metal, not the entire app.
# The repository's native client build lane is macos-14; use that evidenced
# baseline rather than claiming unqualified compatibility with macOS 11-13.
export MACOSX_DEPLOYMENT_TARGET=14.0
target=aarch64-apple-darwin
build_root="$project_root/target/macos-release"
output=${output:-"$project_root/target/distribution"}
mkdir -p "$build_root" "$output"
staging=$(mktemp -d "$build_root/staging.XXXXXX")
cleanup() {
    # Only this invocation's mktemp directory is disposable; retain Cargo cache.
    rm -rf -- "$staging"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cd "$project_root"
cargo build --locked --release --target "$target" --target-dir "$build_root/product" \
    -p compi-client -p compi-daemon -p compi-update --bins

app="$staging/image/Compi.app"
macos="$app/Contents/MacOS"
resources="$app/Contents/Resources"
mkdir -p "$macos" "$resources"
for executable in compi compi-daemon compi-update-worker; do
    source="$build_root/product/$target/release/$executable"
    [[ -x "$source" ]] || { printf 'Missing executable: %s\n' "$source" >&2; exit 1; }
    [[ $(lipo -archs "$source") == arm64 ]] || { printf 'Not ARM64: %s\n' "$source" >&2; exit 1; }
    ditto "$source" "$macos/$executable"
done
ditto "$project_root/LICENSE" "$resources/LICENSE"

iconset="$staging/Compi.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$project_root/assets/Compi-desktopappicon-v4.png" \
        --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z "$((size * 2))" "$((size * 2))" "$project_root/assets/Compi-desktopappicon-v4.png" \
        --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$resources/Compi.icns"

# Apple bundle versions must be numeric even when the Cargo version has a suffix.
bundle_version=${version%%[-+]*}
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>Compi</string>
    <key>CFBundleName</key><string>Compi</string>
    <key>CFBundleExecutable</key><string>compi</string>
    <key>CFBundleIdentifier</key><string>com.compi.app</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$bundle_version</string>
    <key>CFBundleVersion</key><string>$bundle_version</string>
    <key>CFBundleIconFile</key><string>Compi.icns</string>
    <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
    <key>NSHumanReadableCopyright</key><string>Compi contributors. MIT License.</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"

# Seal every nested executable before the bundle. Developer ID uses hardened runtime
# and secure timestamps; ad-hoc signatures do not establish Gatekeeper trust.
if [[ -n "$signing_identity" ]]; then
    for executable in "$macos/"*; do
        codesign --force --options runtime --timestamp --sign "$signing_identity" "$executable"
    done
    codesign --force --options runtime --timestamp --sign "$signing_identity" "$app"
else
    printf 'Ad-hoc signatures; not notarized or Gatekeeper-qualified.\n' >&2
    for executable in "$macos/"*; do
        codesign --force --sign - --timestamp=none "$executable"
    done
    codesign --force --sign - --timestamp=none "$app"
fi
codesign --verify --deep --strict --verbose=2 "$app"

zip_name="Compi-$version-macOS-arm64.app.zip"
dmg_name="Compi-$version-macOS-arm64.dmg"
if [[ -n "$signing_identity" ]]; then
    # Submit the complete bundle, staple it, then regenerate the shipped ZIP.
    ditto -c -k --sequesterRsrc --keepParent "$app" "$staging/notarization.zip"
    xcrun notarytool submit "$staging/notarization.zip" "${notary_arguments[@]}" --wait
    xcrun stapler staple "$app"
    xcrun stapler validate "$app"
    spctl --assess --type execute --verbose=4 "$app"
fi
ditto -c -k --sequesterRsrc --keepParent "$app" "$staging/$zip_name"
ln -s /Applications "$staging/image/Applications"
hdiutil create -volname "Compi $version" -srcfolder "$staging/image" \
    -format UDZO -fs HFS+ "$staging/$dmg_name"
hdiutil verify "$staging/$dmg_name"
if [[ -n "$signing_identity" ]]; then
    codesign --force --timestamp --sign "$signing_identity" "$staging/$dmg_name"
    xcrun notarytool submit "$staging/$dmg_name" "${notary_arguments[@]}" --wait
    xcrun stapler staple "$staging/$dmg_name"
    xcrun stapler validate "$staging/$dmg_name"
    spctl --assess --type open --context context:primary-signature --verbose=4 "$staging/$dmg_name"
fi
(
    cd "$staging"
    shasum -a 256 "$zip_name" "$dmg_name" > SHA256SUMS.txt
)
# Replace only this version's outputs; never clear another platform's artifacts.
mv -f "$staging/$zip_name" "$staging/$dmg_name" "$staging/SHA256SUMS.txt" "$output/"
printf 'App ZIP: %s/%s\nDMG: %s/%s\nChecksums: %s/SHA256SUMS.txt\n' \
    "$output" "$zip_name" "$output" "$dmg_name" "$output"
