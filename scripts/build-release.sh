#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

target=x86_64-unknown-linux-musl
version=$(cargo metadata --locked --no-deps --format-version 1 | jq -er '.packages[] | select(.name == "aur-step") | .version')
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
cargo build --release --locked --target "$target"
binary="target/$target/release/aur-step"
if readelf -lW "$binary" | grep -q INTERP || readelf -dW "$binary" | grep -q NEEDED; then
  echo 'Release binary is not fully static.' >&2
  exit 1
fi
[[ $("$binary" --version) == "aur-step $version" ]]

output="$PWD/target/release-dist"
mkdir -p "$output"
stage=$(mktemp -d "$output/.stage.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT
install -m 755 "$binary" "$stage/aur-step"
install -m 644 LICENSE-MIT LICENSE-APACHE "$stage/"
archive="aur-step-v$version-$target.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner \
  --mtime="@$(git show -s --format=%ct HEAD)" \
  -czf "$output/$archive" -C "$stage" aur-step LICENSE-MIT LICENSE-APACHE
(cd "$output" && sha256sum "$archive" > SHA256SUMS)
printf 'Static release: %s/%s\nSign SHA256SUMS before publishing.\n' "$output" "$archive"
