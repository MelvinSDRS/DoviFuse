#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

if [[ ${GITHUB_REF:-} != refs/heads/main || ! ${GITHUB_SHA:-} =~ ^[0-9a-f]{40}$ ]]; then
  echo "Release publication requires an exact main commit." >&2
  exit 1
fi
case ${RELEASE_CHANNEL:-experimental} in
  experimental) channel=experimental; prerelease=true; latest=false ;;
  stable) channel=stable; prerelease=false; latest=true ;;
  *) echo "Unknown release channel." >&2; exit 1 ;;
esac
if [[ $channel == stable ]]; then
  python3 "$ROOT/scripts/verify_reference_catalog.py" --release
fi

assets=(DV8-Maker-arm64.dmg DV8-Maker-arm64.dmg.sha256
        DV8-Maker-sources.tar.gz DV8-Maker-sources.tar.gz.sha256)
for asset in "${assets[@]}"; do
  [[ -s dist/$asset ]] || { echo "Missing release asset: $asset" >&2; exit 1; }
done
(cd dist && sha256sum --check DV8-Maker-arm64.dmg.sha256 DV8-Maker-sources.tar.gz.sha256)

tag="${channel}-${GITHUB_SHA}"
title="DV8 Maker ${channel} ${GITHUB_SHA:0:7}"
notes=$(mktemp)
trap 'rm -f "$notes"' EXIT
{
  echo "Apple Silicon macOS build from commit ${GITHUB_SHA}."
  echo
  if [[ $channel == experimental ]]; then
    echo 'Experimental download: automated build and regression checks passed; full-length and Dolby playback acceptance remain pending. This is not a stable release.'
    echo
  fi
  echo 'Download DV8-Maker-arm64.dmg, open it, and drag DV8 Maker into Applications.'
  echo 'The app is ad-hoc signed and not notarized. macOS may require you to allow it in Privacy & Security before opening.'
  echo
  echo 'Use copies of your media. Standard conversion replaces the input after validation; hybrid conversion keeps both sources.'
  echo
  echo 'SHA-256 files verify the downloads. DV8-Maker-sources.tar.gz contains the release source inventory, bundled upstream source archives, and build instructions. Third-party components retain their own licenses.'
} > "$notes"

# Stage all assets before exposing a new release. Retrying a failed draft is safe;
# already published commit releases keep their original downloads.
if draft=$(gh release view "$tag" --json isDraft --jq '.isDraft' 2>/dev/null); then
  if [[ $draft == false ]]; then
    echo "Release already published: $tag"
    exit 0
  fi
else
  gh release create "$tag" --target "$GITHUB_SHA" --title "$title" \
    --draft --prerelease="$prerelease" --latest=false --notes-file "$notes"
fi
paths=()
for asset in "${assets[@]}"; do paths+=("dist/$asset"); done
gh release upload "$tag" "${paths[@]}" --clobber
gh release edit "$tag" --draft=false --prerelease="$prerelease" \
  --latest="$latest" --title "$title" --notes-file "$notes"
gh release view "$tag" --json url --jq '.url'
