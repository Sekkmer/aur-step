# Usage Examples

## Initialize

```bash
sudo aur-step init
sudo aur-step status --json
```

## Import Existing Yay State

```bash
sudo aur-step import-yay --json
```

This reads `pacman -Qm`, matches installed foreign packages to Yay checkouts
under the configured `yay_build_dir`, and records package state without trusting
the checkouts as reviewed.

## Inspect and Review

```bash
sudo aur-step fetch visual-studio-code-bin
sudo aur-step inspect visual-studio-code-bin --json
sudo aur-step review visual-studio-code-bin
```

Review records the current git commit. Later fetches that advance the checkout
make `reviewed=false` until review is recorded again.

Read persistent provenance and the action journal with:

```bash
sudo aur-step audit visual-studio-code-bin --json
```

## Dependencies

```bash
sudo aur-step deps visual-studio-code-bin --json
sudo aur-step deps --recursive visual-studio-code-bin --json
```

Provider dependencies require explicit selection:

```bash
sudo aur-step install-repo-deps --plan --provider ttf-font=noto-fonts brave-bin
sudo aur-step install-repo-deps --provider ttf-font=noto-fonts brave-bin
```

## Install

Default install stops after fetch and dependency classification if review is
required:

```bash
sudo aur-step install visual-studio-code-bin --json
```

After review, install can proceed without bypassing the gate:

```bash
sudo aur-step review visual-studio-code-bin
sudo aur-step install visual-studio-code-bin --json
```

For an exact one-run automation grant:

```bash
sudo aur-step install \
  --reviewed-commit visual-studio-code-bin=<full-sha> \
  visual-studio-code-bin --json
```

Source findings and maintainer transitions require deliberate review:

```bash
sudo aur-step review --allow-high-risk visual-studio-code-bin
sudo aur-step review --allow-maintainer-change visual-studio-code-bin
```

Packages that intentionally ship services, hooks, install scripts, or other
privileged integration require a separate artifact approval:

```bash
sudo aur-step install-built --allow-privileged-files visual-studio-code-bin
sudo aur-step install \
  --allow-privileged-files visual-studio-code-bin \
  visual-studio-code-bin
```

Recursive AUR dependencies remain explicit:

```bash
sudo aur-step install --auto-aur-deps --provider ttf-font=noto-fonts some-package --json
```

## Upgrade

Read-only plan:

```bash
sudo aur-step upgrade --plan --json
```

Refresh metadata as the build user, but do not build or install:

```bash
sudo aur-step upgrade --plan --refresh --json
```

Execute reviewed ready upgrades:

```bash
sudo aur-step upgrade --provider ttf-font=noto-fonts --json
```

Current packages report `no_action`. Upgrade candidates that are not reviewed or
have unresolved metadata report `blocked`.

## Clean

```bash
sudo aur-step clean visual-studio-code-bin --json
```

This removes generated build outputs only: `src/`, `pkg/`, and package archives.
It preserves `PKGBUILD`, `.SRCINFO`, `.git`, signatures, and state.

## Remove

```bash
sudo aur-step remove --plan visual-studio-code-bin --json
sudo aur-step remove visual-studio-code-bin --json
```

Non-plan remove runs `pacman -Rns --noconfirm` and deletes `aur-step` state only
after pacman succeeds.
