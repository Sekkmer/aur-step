# Signed static releases

Releases provide `aur-step-vVERSION-x86_64-unknown-linux-musl.tar.gz`, containing
a fully static executable and the MIT/Apache license texts. External Arch tools
(`pacman`, `makepkg`, `git`, `curl`, `bubblewrap`, and `bsdtar`) are still needed.

The release tag and `SHA256SUMS` are signed by Sekkmer:

`BFBC402DFF513BC806F63409BA6C5BE10CF2708B`

The public key is in `packaging/release-signing-key.asc`. Verify the fingerprint
through a trusted channel before using it for the first installation.

## Build and publish

Install the Rust musl target, `musl`, `binutils`, and `jq`, then run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
bash scripts/build-release.sh
```

The build script rejects dynamically linked binaries and checks `--version`.
Commit the reviewed changes, then rebuild from that commit. Sign the tag and
manifest with the same key (replace the version for subsequent releases):

```bash
git tag -s -u BFBC402DFF513BC806F63409BA6C5BE10CF2708B v0.1.0 -m 'aur-step v0.1.0'
gpg --local-user BFBC402DFF513BC806F63409BA6C5BE10CF2708B --armor --detach-sign target/release-dist/SHA256SUMS
git verify-tag v0.1.0
git push origin main v0.1.0
gh release create v0.1.0 --verify-tag --title 'aur-step v0.1.0' \
  target/release-dist/aur-step-v0.1.0-x86_64-unknown-linux-musl.tar.gz \
  target/release-dist/SHA256SUMS target/release-dist/SHA256SUMS.asc
```

Publish all three assets together. The updater rejects incomplete releases,
invalid signatures, mismatched checksums, and version mismatches.

## Install and enable updates on Arch

From a verified checkout:

```bash
sudo pacman -S --needed base-devel git curl bubblewrap libarchive jq gnupg
sudo install -Dm644 packaging/release-signing-key.asc /usr/local/share/aur-step/release-signing-key.asc
sudo install -Dm755 scripts/aur-step-update /usr/local/sbin/aur-step-update
sudo install -Dm644 packaging/aur-step-update.service /etc/systemd/system/aur-step-update.service
sudo install -Dm644 packaging/aur-step-update.timer /etc/systemd/system/aur-step-update.timer
sudo aur-step-update
sudo systemctl daemon-reload
sudo systemctl enable --now aur-step-update.timer
```

Configure `/etc/aur-step.toml` and run `sudo aur-step init` as described in the
README. Use `aur-step-update --check` for a read-only version check, or
`sudo systemctl start aur-step-update` for an immediate update.

The timer checks GitHub's latest stable release daily, refuses downgrades, and
replaces only `/usr/local/bin/aur-step`, atomically. It does not upgrade AUR
packages or grant package review approvals. Updater scripts, units, and signing
key rotations are installed explicitly from a verified checkout.

For a private repository, supply an optional root-owned mode-0600
`/etc/aur-step-release.headers` containing `Authorization: Bearer YOUR_TOKEN`.
Use a fine-grained token limited to read-only Contents access on this repository.
The token is read from that file rather than put in process arguments. Public
repositories need no credentials.

To disable updates: `sudo systemctl disable --now aur-step-update.timer`.
