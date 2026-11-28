#!/usr/bin/env bash
set -euo pipefail

# Standalone Developer ID signing, Hardened Runtime notarization, stapling,
# and .dmg packaging for Neutron.app on macOS.
#
# Usage:
#   scripts/notarize-macos.sh [path/to/Neutron.app] [--identity "<id>"] [--profile "<name>"] [--no-dmg] [--sign-only]

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
ENTITLEMENTS="${NEUTRON_ENTITLEMENTS:-${ROOT_DIR}/assets/entitlements.plist}"

APP_PATH=""
IDENTITY="${NEUTRON_SIGN_IDENTITY:-}"
PROFILE="${NEUTRON_NOTARY_PROFILE:-neutron-notary}"
BUILD_DMG=1
SIGN_ONLY=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --identity)
      IDENTITY="$2"
      shift 2
      ;;
    --profile)
      PROFILE="$2"
      shift 2
      ;;
    --no-dmg)
      BUILD_DMG=0
      shift
      ;;
    --sign-only)
      SIGN_ONLY=1
      shift
      ;;
    -h|--help)
      sed -n '3,10p' "$0"
      exit 0
      ;;
    -*)
      echo "unknown option: $1" >&2
      exit 2
      ;;
    *)
      if [[ -z "${APP_PATH}" ]]; then
        APP_PATH="$1"
      else
        echo "unexpected argument: $1" >&2
        exit 2
      fi
      shift
      ;;
  esac
done

if [[ -z "${APP_PATH}" ]]; then
  if [[ -d "${ROOT_DIR}/target/release/Neutron.app" ]]; then
    APP_PATH="${ROOT_DIR}/target/release/Neutron.app"
  else
    APP_PATH="${HOME}/Applications/Neutron.app"
  fi
fi

if [[ ! -d "${APP_PATH}" ]]; then
  echo "error: Neutron.app not found at ${APP_PATH}" >&2
  exit 1
fi
if [[ ! -f "${ENTITLEMENTS}" ]]; then
  echo "error: entitlements not found at ${ENTITLEMENTS}" >&2
  exit 1
fi

if [[ -z "${IDENTITY}" ]]; then
  IDS=()
  while IFS= read -r line; do
    if [[ "${line}" == *"Developer ID Application:"* ]]; then
      id="$(printf '%s\n' "${line}" | sed -n 's/.*"\(Developer ID Application: [^"]*\)".*/\1/p')"
      [[ -n "${id}" ]] && IDS+=("${id}")
    fi
  done < <(/usr/bin/security find-identity -v -p codesigning)
  if [[ ${#IDS[@]} -eq 1 ]]; then
    IDENTITY="${IDS[0]}"
  elif [[ ${#IDS[@]} -eq 0 ]]; then
    echo "error: no Developer ID Application identity found; pass --identity" >&2
    exit 1
  else
    echo "error: multiple Developer ID identities found; pass --identity" >&2
    exit 1
  fi
fi

echo "==> Signing ${APP_PATH} with: ${IDENTITY}"
for target in \
  "${APP_PATH}/Contents/MacOS/neutron-macos" \
  "${APP_PATH}/Contents/MacOS/neutron" \
  "${APP_PATH}"; do
  /usr/bin/codesign --force --options runtime --timestamp \
    --entitlements "${ENTITLEMENTS}" \
    --sign "${IDENTITY}" "${target}"
done

/usr/bin/codesign --verify --deep --strict --verbose=2 "${APP_PATH}"
/usr/bin/codesign -dvv "${APP_PATH}" 2>&1 | grep -E "Authority=|TeamIdentifier=|flags=" || true

if [[ ${SIGN_ONLY} -eq 1 ]]; then
  echo "==> Sign-only complete."
  exit 0
fi

APP_PARENT="$(cd "$(dirname "${APP_PATH}")" && pwd)"
ZIP_PATH="${APP_PARENT}/Neutron-notarize.zip"
rm -f "${ZIP_PATH}"

echo "==> Submitting app archive to Apple notary service (profile: ${PROFILE})"
/usr/bin/ditto -c -k --keepParent "${APP_PATH}" "${ZIP_PATH}"
trap 'rm -f "${ZIP_PATH}"' EXIT
/usr/bin/xcrun notarytool submit "${ZIP_PATH}" --keychain-profile "${PROFILE}" --wait
rm -f "${ZIP_PATH}"

echo "==> Stapling and validating ${APP_PATH}"
/usr/bin/xcrun stapler staple "${APP_PATH}"
/usr/bin/xcrun stapler validate "${APP_PATH}"
/usr/sbin/spctl -a -t execute -v "${APP_PATH}" || true

if [[ ${BUILD_DMG} -eq 1 ]]; then
  VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "${APP_PATH}/Contents/Info.plist" 2>/dev/null || echo '0.1.0')"
  ARCH="$(uname -m)"
  DMG_PATH="${APP_PARENT}/Neutron-${VERSION}-${ARCH}.dmg"
  DMG_STAGE="$(mktemp -d "${TMPDIR:-/tmp}/neutron-dmg.XXXXXX")"
  trap 'rm -rf "${DMG_STAGE}"' EXIT

  echo "==> Creating DMG at ${DMG_PATH}"
  /usr/bin/ditto "${APP_PATH}" "${DMG_STAGE}/Neutron.app"
  ln -s /Applications "${DMG_STAGE}/Applications"
  rm -f "${DMG_PATH}"
  /usr/bin/hdiutil create -volname "Neutron" -srcfolder "${DMG_STAGE}" -ov -format UDZO "${DMG_PATH}"
  rm -rf "${DMG_STAGE}"

  echo "==> Signing, notarizing, and stapling ${DMG_PATH}"
  /usr/bin/codesign --force --timestamp --sign "${IDENTITY}" "${DMG_PATH}"
  /usr/bin/xcrun notarytool submit "${DMG_PATH}" --keychain-profile "${PROFILE}" --wait
  /usr/bin/xcrun stapler staple "${DMG_PATH}"
  /usr/bin/xcrun stapler validate "${DMG_PATH}"
  /usr/sbin/spctl -a -t open --context context:primary-signature -v "${DMG_PATH}"
  echo "==> Done: ${DMG_PATH}"
fi
