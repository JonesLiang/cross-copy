#!/bin/bash
set -euo pipefail

app_path="${1:?usage: sign-macos-app.sh /path/to/CrossCopy.app}"
bundle_id="app.crosscopy.desktop"

if [[ ! -d "$app_path" ]]; then
  echo "CrossCopy app bundle not found: $app_path" >&2
  exit 1
fi

team_id="$(
  codesign -dv --verbose=4 "$app_path" 2>&1 \
    | sed -n 's/^TeamIdentifier=//p' \
    | head -n 1
)"

if [[ -n "$team_id" && "$team_id" != "not set" ]]; then
  echo "Keeping certificate-backed signature for team $team_id"
else
  # GitHub's unsigned Tauri bundle otherwise receives a changing cdhash-only
  # identity. A stable designated requirement lets macOS associate the user's
  # one-time Accessibility decision with future CrossCopy builds.
  codesign \
    --force \
    --deep \
    --sign - \
    --identifier "$bundle_id" \
    --requirements "=designated => identifier \"$bundle_id\"" \
    "$app_path"
fi

actual_id="$(codesign -dv --verbose=4 "$app_path" 2>&1 | sed -n 's/^Identifier=//p' | head -n 1)"
if [[ "$actual_id" != "$bundle_id" ]]; then
  echo "Unexpected signing identifier: $actual_id" >&2
  exit 1
fi

codesign --verify --deep --strict --verbose=2 "$app_path"
codesign -dr - "$app_path"
