# aur-step

`aur-step` is a root-supervised AUR orchestration tool for automation.

It is not meant to be a full `yay` or `paru` clone. Its job is to provide
explicit, deterministic AUR operations while ensuring AUR build code runs as a
configured unprivileged user.

Primary use case:

```bash
sudo aur-step install visual-studio-code-bin
sudo aur-step install --assume-reviewed visual-studio-code-bin
sudo aur-step install --assume-reviewed --auto-aur-deps some-package
sudo aur-step upgrade
```

Yay migration/import:

```bash
sudo aur-step import-yay --json
```

This reads installed foreign packages from `pacman -Qm`, matches them against
Yay checkouts under `~/.cache/yay`, and records them in `aur-step` state. The
goal is compatibility with existing Yay-managed systems, not delegating package
build/install decisions back to Yay.

## Core Idea

`aur-step` separates privileged and unprivileged work:

```text
root supervisor
  -> installs official repo dependencies with pacman
  -> installs finished packages with pacman -U

configured build user
  -> clones AUR repos
  -> reviews/updates PKGBUILD trees
  -> runs makepkg
  -> executes all PKGBUILD code
```

The tool must never execute `PKGBUILD`, `prepare()`, `build()`, `check()`, or `package()` as root.

## Goals

- Start cleanly from a root supervisor such as an automation agent.
- Drop privileges for all AUR-controlled code.
- Support single-package installs.
- Support updates similar to `yay -Syu`.
- Be mostly automatic, but stop at points where judgment or trust is required.
- Emit machine-readable output so Codex can decide the next step.
- Avoid broad sudo prompts and avoid running AUR helpers as root.
- Keep enough state to know which AUR packages it manages.
- Prefer official repo packages for dependencies whenever available.

## Non-Goals

- Replacing all `yay` or `paru` functionality on day one.
- Running arbitrary AUR build scripts as root.
- Providing a TUI.
- Hiding PKGBUILD review.
- Solving every dependency cycle automatically in the first version.
- Building in a clean chroot in the first version.

## Configuration

Install a root-owned configuration at `/etc/aur-step.toml`:

```toml
build_user = "alice"
```

When omitted, `build_root` defaults to `<build-user-home>/aurbuild`,
`yay_build_dir` defaults to `<build-user-home>/.cache/yay`, and the state
database defaults to `/var/lib/aur-step/state.sqlite3`. See
[`aur-step.toml.example`](aur-step.toml.example) for every field.

As root, `aur-step` rejects configuration and state paths that contain symlinks,
are not root-owned, or are writable by another user. Install a configuration
with:

```bash
sudo install -o root -g root -m 0644 aur-step.toml.example /etc/aur-step.toml
sudoedit /etc/aur-step.toml
```

Without a config file, `build_user` may be supplied through
`AUR_STEP_BUILD_USER` or inferred from `SUDO_USER`. The tool never selects
`root` as the build user.

## Security Boundary

Build commands receive a small environment, no inherited stdin, and no Git
credential prompts. Generated package archives are opened without following
symlinks, checked for build-user ownership and hard links, copied into
root-owned temporary staging, and parsed by `pacman -Qp` before installation.

This is privilege separation, not a build sandbox. A `PKGBUILD` still has the
normal filesystem and network access of the build user. Review AUR changes and
use a clean chroot or stronger sandbox when that threat model requires one.

## Command Shape

High-level commands:

```bash
aur-step install <pkg>...
aur-step upgrade
aur-step status
aur-step inspect <pkg>
aur-step remove <pkg>...
```

Low-level step commands for Codex:

```bash
aur-step fetch <pkg>
aur-step import-yay
aur-step review <pkg>
aur-step deps <pkg>
aur-step install-repo-deps --plan <pkg>
aur-step install-repo-deps <pkg>
aur-step build <pkg>
aur-step install-built --plan <pkg>
aur-step install-built <pkg>
aur-step clean <pkg>
```

`clean` removes generated build outputs for a package checkout: `src/`, `pkg/`,
and recognized package archives. It preserves `PKGBUILD`, `.SRCINFO`, `.git`,
signature files, and `aur-step` state.

`remove --plan <pkg>...` shows the `pacman -Rns --noconfirm` command and which
packages are currently managed in `aur-step` state. Non-plan `remove` runs the
pacman removal as root and deletes package records from state only after pacman
succeeds.

`install` intentionally has a review gate. Without `--assume-reviewed`, it
fetches the AUR tree, refreshes `.SRCINFO`, classifies dependencies, and stops
before installing dependencies or executing the build.

After reviewing the checkout, record the trusted commit:

```bash
aur-step review visual-studio-code-bin
```

Future `install` runs can proceed without `--assume-reviewed` only while the
current git commit still matches `reviewed_commit`. If `fetch` pulls a new
commit, review must be recorded again.

High-level install accepts the same explicit provider selection shape as
`install-repo-deps`:

```bash
aur-step install --provider ttf-font=noto-fonts brave-bin
```

`install --auto-aur-deps` recursively installs confirmed AUR dependencies only
after the current package has passed the same review gate. Without
`--auto-aur-deps`, confirmed AUR dependencies remain a stop point.

`install --json` emits one top-level JSON result with package step outputs in
dependency order.

High-level upgrade accepts provider selections for AUR package dependency
installation:

```bash
aur-step upgrade --provider ttf-font=noto-fonts
```

Selections are applied only to upgraded packages whose dependency plans contain
the matching provider dependency.

`inspect --json` includes `review_diff` when a package has a recorded
`reviewed_commit`. The diff object reports whether the checkout is current or
changed, lists changed files, and includes the git diff from the reviewed commit
to the current commit.

Machine-readable mode:

```bash
aur-step deps visual-studio-code-bin --json
aur-step install-built --plan visual-studio-code-bin --json
aur-step upgrade --plan --json
aur-step upgrade --plan --refresh --json
```

Dependency JSON splits missing non-repo dependencies into `provider_deps`,
confirmed `aur_deps`, and `unknown_deps`. Confirmed AUR dependencies are found
through the configured `aur_url`; `file://` AUR roots are checked locally for
tests and offline fixtures.

Recursive dependency planning is read-only:

```bash
aur-step deps --recursive visual-studio-code-bin --json
```

It walks existing local `.SRCINFO` files for confirmed AUR dependencies and
reports AUR packages whose checkouts still need to be fetched.

Provider dependencies stay blocked unless the caller selects an exact candidate:

```bash
aur-step install-repo-deps --plan --provider ttf-font=noto-fonts brave-bin
aur-step install-repo-deps --provider ttf-font=noto-fonts brave-bin
```

Local/fake AUR repos can be used by setting `aur_url` in config. The default is
`https://aur.archlinux.org`; tests use a `file://...` URL so fetch behavior can
be exercised without network access.

## Update Model

`aur-step upgrade` approximates:

```bash
yay -Syu
```

But internally it should be explicit:

1. Run official repo upgrade:

   ```bash
   pacman -Syu
   ```

2. Discover installed AUR packages.

3. Update AUR git trees.

4. Compare installed package versions against AUR `.SRCINFO` versions.

5. Build packages that changed.

6. Install successfully built packages with:

   ```bash
   pacman -U
   ```

7. Report packages that need manual handling.

The first version may require packages to be registered in an `aur-step` state file. Later versions can infer foreign packages with:

```bash
pacman -Qm
```

Current `upgrade --plan` behavior is read-only:

1. Read `aur-step` state.
2. Read installed foreign package versions with `pacman -Qm`.
3. Fall back to stored state versions if `pacman -Qm` is unavailable.
4. Parse existing `.SRCINFO` files.
5. Compare versions with `vercmp`.
6. Report statuses such as `current`, `upgrade_available`,
   `newer_than_srcinfo`, `missing_srcinfo`, and `not_installed`.
7. For each `upgrade_available` package, report whether the current checkout is
   reviewed and therefore ready for the future build/install sequence.

It does not update git checkouts or build packages yet. For VCS packages, cached
`.SRCINFO` can lag behind the version actually installed, so `newer_than_srcinfo`
is common until the checkout is refreshed.

`upgrade --plan --refresh` refreshes metadata before comparing versions:

1. Run `git pull --ff-only` in existing git checkouts as the build user.
2. Regenerate `.SRCINFO` with `makepkg --printsrcinfo` as the build user.
3. Report old/new commit, `metadata_refreshed`, `refresh_error`,
   `ready_for_build`, `planned_actions`, and `build_blocked_reasons` per
   package.

It still does not build packages or invoke `pacman -U`.

Non-plan `aur-step upgrade` executes the conservative path:

1. Run `pacman -Syu --noconfirm` as root.
2. Refresh managed AUR checkouts as the build user.
3. Execute only packages whose plan is `ready_for_build=true`.
4. For each ready package, run `deps`, `install-repo-deps`, `build`, and
   `install-built`.
5. Report completed, no-action, blocked, and failed packages in the top-level
   result.

## Current Design Docs

See:

- [docs/requirements.md](docs/requirements.md)
- [docs/state-and-safety.md](docs/state-and-safety.md)
- [docs/json-contracts.md](docs/json-contracts.md)
- [docs/usage-examples.md](docs/usage-examples.md)
- [docs/roadmap.md](docs/roadmap.md)

## Testing

The default test suite never changes the host package database, even when it is
run as root:

```bash
cargo test --all-targets
```

Four root integration tests exercise privilege dropping, package building, and
temporary package installation. Run them only in an isolated Arch environment:

```bash
sudo AUR_STEP_RUN_ROOT_INTEGRATION=1 \
  AUR_STEP_ROOT_TEST_USER=alice \
  cargo test --test fake_aur root_ -- --test-threads=1
```

The install test uses a uniquely named fake package and a drop guard to remove
it if the test returns early or an assertion unwinds.

## License

Licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in `aur-step` is dual-licensed as above, without additional terms
or conditions.
