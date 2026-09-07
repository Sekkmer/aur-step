#!/usr/bin/env bash
# Exercise the real GPG/checksum verifier without root or network access.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
repo=$PWD
release_dir=${1:?Pass the signed release output directory}
archive=${2:?Pass the release archive filename}
key="$repo/packaging/release-signing-key.asc"
stage=$(mktemp -d /tmp/aur-step-release-test.XXXXXXXX)
trap 'rm -rf -- "$stage"' EXIT

fixture() {
  mkdir "$stage/$1"
  cp "$release_dir/SHA256SUMS" "$release_dir/SHA256SUMS.asc" "$release_dir/$archive" "$stage/$1/"
}
verify() {
  bash -c 'source "$1"; verify_release "$2" "$3" "$4"' \
    bash "$repo/scripts/aur-step-update" "$stage/$1" "$archive" "$key"
}

fixture valid
verify valid
fixture bad-signature
truncate -s 0 "$stage/bad-signature/SHA256SUMS"
if verify bad-signature; then
  echo 'ERROR: a modified checksum manifest was accepted.' >&2
  exit 1
fi
fixture bad-checksum
truncate -s 1 "$stage/bad-checksum/$archive"
if verify bad-checksum; then
  echo 'ERROR: a corrupted archive was accepted.' >&2
  exit 1
fi
echo 'Release verification passed: valid signature accepted; tampered manifest and archive rejected.'
