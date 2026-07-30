# aur-step Requirements

## Problem

Automation may start as root. Existing AUR helpers such as `yay` and `paru`
correctly refuse to run as root. Running them entirely as an unprivileged user
creates a second problem: dependency and final package installation need a
carefully limited privileged step.

`aur-step` should bridge that workflow safely:

```text
Codex/root starts the command
root does root-only pacman operations
the configured build user does all AUR fetching and building
Codex/root receives structured results
```

## Security Invariants

These must always hold:

- Never run AUR-controlled shell code as root.
- Never run `makepkg` as root.
- Never pass root secrets into the build environment.
- Never expose `~/.ssh`, `~/.gnupg`, `~/.docker`, browser profiles, Codex auth, or KeePass files to AUR builds.
- Build as the configured unprivileged user in a user-owned build directory.
- Install only finished package archives as root using `pacman -U`.
- Use official `pacman` for official repo dependencies.
- Stop and report when a dependency cannot be classified safely.

## Paths

Default paths:

```text
Build root:   <build-user-home>/aurbuild
State dir:    /var/lib/aur-step
Config file:  /etc/aur-step.toml
Log dir:      /var/log/aur-step
Build user:   configured non-root account
```

State should not live in the build user's home, because root needs a trustworthy
record of what the supervisor thinks it manages.

## Package Install Workflow

Command:

```bash
aur-step install visual-studio-code-bin
```

Required flow:

1. Validate caller is root.
2. Validate build user exists and is not root.
3. Create `<build-user-home>/aurbuild`.
4. Clone or update:

   ```text
   https://aur.archlinux.org/visual-studio-code-bin.git
   ```

   as the configured build user.

5. Generate or refresh `.SRCINFO` if needed as the build user.
6. Parse `.SRCINFO`.
7. Classify dependencies:

   ```text
   official repo installed
   official repo missing
   AUR dependency
   unknown/unresolved dependency
   ```

8. If repo dependencies are missing, install them as root:

   ```bash
   pacman -S --needed <deps>
   ```

9. If AUR dependencies exist, either:

   - recurse if `--auto-aur-deps` is enabled and each fetched package passes the
     review gate, or
   - stop and emit them for Codex to handle.

10. Show review summary:

    ```text
    package name
    version
    source URL
    maintainer info if available
    changed PKGBUILD diff if already cloned
    dependencies
    install target package files
    ```

11. Build as the configured build user:

    ```bash
    makepkg --noconfirm --needed
    ```

    Do not use `makepkg -s` unless dependency installation has been disabled or controlled.

12. Install built package as root:

    ```bash
    pacman -U --needed <root-owned-staging>/*.pkg.tar.zst
    ```

13. Record package in state.

Current implementation note:

```text
aur-step install <pkg>
```

fetches and classifies, then stops for review.

```text
aur-step install --assume-reviewed <pkg>
```

continues through repo dependency installation, user build, and root package
installation.

Current high-level install behavior:

- `install --provider dependency=package <pkg>` forwards explicit provider
  selections to the relevant dependency install step.
- `install --auto-aur-deps <pkg>` recursively installs confirmed AUR
  dependencies before the package that needs them.
- Recursive AUR dependency installation still honors review gating for each
  fetched package unless `--assume-reviewed` is set.
- Without `--auto-aur-deps`, confirmed AUR dependencies remain a stop point.
- `install --json` emits one top-level result containing package step outputs in
  dependency order.

## Yay Compatibility

`aur-step` should be able to bootstrap from a system that was previously managed
with Yay.

Required behavior:

- Read Yay's effective config with `yay -Pg` when available.
- Default Yay build/cache path:

  ```text
  <build-user-home>/.cache/yay
  ```

- Import currently installed foreign packages from:

  ```bash
  pacman -Qm
  ```

- If `~/.cache/yay/<pkg>` exists and contains `PKGBUILD` or `.SRCINFO`, record
  that checkout as the initial build path.
- If an installed foreign package has no matching Yay checkout, still import it
  using `aur-step`'s normal build path so it can be fetched/reviewed later.
- Report cache-only Yay directories separately. They should not be treated as
  installed packages unless explicitly requested.

Compatibility does not mean using Yay as the backend. After import, `aur-step`
should keep enforcing its own root/user split and review gates.

## Upgrade Workflow

Command:

```bash
aur-step upgrade
```

Desired behavior similar to:

```bash
yay -Syu
```

Required flow:

1. Refresh and upgrade official repos:

   ```bash
   pacman -Syu
   ```

2. Determine AUR packages to check.

   Version 1:

   ```text
   Use aur-step state file.
   ```

   Version 2:

   ```text
   Combine aur-step state with pacman -Qm foreign package list.
   ```

3. For each AUR package:

   - update git checkout as the configured build user,
   - parse `.SRCINFO`,
   - compare installed version with AUR version,
   - mark as current, upgrade-needed, missing, renamed, or error.

4. Build all upgrade-needed packages as the configured build user.
5. Install successful builds as root.
6. Report failures without blocking unrelated successful packages unless `--fail-fast` is set.

`aur-step upgrade --plan` should not modify anything except optionally refreshing metadata. It should output a plan.

Current implementation is stricter: `upgrade --plan` is read-only and does not
refresh AUR git checkouts. It plans from existing state, `pacman -Qm`, and
existing `.SRCINFO` files.

`upgrade --plan --refresh` may update AUR git checkouts and regenerate
`.SRCINFO`, but it must still avoid builds and package installation.

Current upgrade plans also expose build readiness:

- `ready_for_build` is true only for `upgrade_available` packages whose current
  checkout commit matches `reviewed_commit`.
- `planned_actions` lists the future low-level sequence:
  `deps`, `install-repo-deps`, `build`, `install-built`.
- `build_blocked_reasons` explains why an upgrade package is not ready, such as
  missing `.SRCINFO`, refresh errors, or an unreviewed checkout.

Current non-plan `upgrade` execution:

- runs `pacman -Syu --noconfirm` as root first,
- refreshes managed AUR checkouts and `.SRCINFO` as the build user,
- executes only packages with `ready_for_build=true`,
- accepts repeated `--provider dependency=package` selections and applies them
  to matching package dependency plans,
- runs the same dependency, build, and artifact-install primitives as the
  low-level commands,
- reports completed, no-action, blocked, and failed packages in one top-level
  result,
- exits successfully when packages are merely current/no-action,
- exits with failure when any package is blocked or failed.

## Dependency Handling

Required for version 1:

- Parse `.SRCINFO`.
- Handle:

  ```text
  depends
  depends_<arch>
  makedepends
  makedepends_<arch>
  checkdepends
  checkdepends_<arch>
  optdepends as informational only
  optdepends_<arch> as informational only
  ```

- Strip simple version constraints for lookup:

  ```text
  foo>=1.0 -> foo
  bar=2.0  -> bar
  ```

- Use `pacman -T` to find missing deps.
- Use `pacman -Si` to classify official repo deps.
- Resolve remaining deps against the configured AUR URL.
- Report confirmed AUR deps separately from unknown deps.

Current dependency behavior intentionally treats arch-specific `.SRCINFO`
dependency fields as active for the local plan rather than silently ignoring
them.

Required later:

- Split package handling.
- Conflict/replaces summaries.
- Better version constraint validation.

Current provider behavior:

- If a dependency is not installed and is not an exact sync repo package,
  `aur-step` checks local pacman sync metadata for packages that `Provides` it.
- Provider dependencies are reported separately from AUR/unknown dependencies.
- Provider dependencies are still a stop point for install/build automation
  unless a provider is explicitly selected.
- Provider selection uses repeated `--provider dependency=package` arguments on
  `install-repo-deps`.
- A selected provider must match one of the reported candidates for that
  dependency.
- Selected providers are added to the `pacman -S --needed --noconfirm` package
  list and reported in `selected_providers`.

Current AUR dependency classification:

- Dependencies missing from the local system and official repos are checked
  against the configured `aur_url`.
- `file://` AUR roots are resolved locally, which keeps fake-AUR tests
  deterministic.
- Non-file AUR roots are checked with `git ls-remote --exit-code`.
- Confirmed AUR deps are emitted in `aur_deps`.
- Deps that cannot be resolved as repo, provider, or AUR packages are emitted in
  `unknown_deps` and the compatibility field `aur_or_unknown_deps`.

Current recursive dependency planning:

- `aur-step deps --recursive <pkg>` is read-only.
- It starts from the package's existing local `.SRCINFO`.
- Confirmed AUR deps with existing local `.SRCINFO` files are inspected
  recursively.
- Confirmed AUR deps without local `.SRCINFO` are reported with
  `status=needs_fetch`.
- Cycles are reported with `status=already_seen`.
- The command does not clone, build, install repo dependencies, or mutate state.

## Review Behavior

Default behavior should be safe for Codex:

```text
Automatic mechanical steps are okay.
Trust decisions should be visible.
```

For new packages, default to requiring review unless `--assume-reviewed` is passed.

Review output should include:

```text
PKGBUILD path
.SRCINFO path
git remote URL
last commit
diff since last aur-step build
sources
install scripts
pkgver/pkgrel
dependencies
```

Current review persistence:

```bash
aur-step review <pkg>
```

records the current checkout commit as `reviewed_commit` in state. High-level
`install` may proceed without `--assume-reviewed` only if the current checkout
commit still matches that reviewed commit.

Current `inspect --json` review output:

- `reviewed` is true only when the current git commit equals `reviewed_commit`.
- `review_diff` is present when a reviewed commit exists.
- `review_diff.status` is `current`, `changed`, `missing_current_commit`, or
  `error`.
- For changed checkouts, `review_diff` includes changed files, `git diff --stat`,
  and the full git diff from reviewed commit to current commit.

## JSON Output

Every command should support:

```bash
--json
```

Example dependency output:

```json
{
  "package": "visual-studio-code-bin",
  "repo_deps_installed": ["glibc"],
  "repo_deps_missing": ["libxkbfile"],
  "provider_deps": [],
  "selected_providers": [],
  "aur_deps": ["some-aur-helper-lib"],
  "unknown_deps": [],
  "aur_or_unknown_deps": [],
  "optdepends": ["libdbusmenu-glib: KDE global menu support"]
}
```

Example recursive dependency package status:

```json
{
  "root": "example",
  "inspected_count": 2,
  "needs_fetch_count": 1,
  "packages": [
    {
      "package": "example-aur-dep",
      "required_by": "example",
      "status": "needs_fetch",
      "dependency_plan": null
    }
  ]
}
```

Commands that can invoke `pacman` should support `--plan` before they mutate the
system package database.

Current remove behavior:

- `aur-step remove --plan <pkg>...` emits `pacman -Rns --noconfirm <pkg>...`
  arguments and whether each package is managed by `aur-step`.
- `aur-step remove <pkg>...` requires root, runs pacman, and deletes matching
  package records from state only after successful removal.
- Removing packages does not delete AUR checkout directories; use `clean` for
  generated build outputs.

Example upgrade plan:

```json
{
  "package_count": 1,
  "upgrade_count": 1,
  "ready_for_build_count": 1,
  "repo_upgrade_required": true,
  "packages": [
    {
      "package": "visual-studio-code-bin",
      "status": "upgrade_available",
      "installed_version": "1.101.0-1",
      "available_version": "1.102.0-1",
      "reviewed": true,
      "ready_for_build": true,
      "planned_actions": ["deps", "install-repo-deps", "build", "install-built"],
      "build_blocked_reasons": []
    }
  ]
}
```

## Exit Codes

Suggested exit codes:

```text
0   success
1   general error
2   not root when root required
3   unsafe operation refused
4   review required
5   missing official repo dependencies
6   AUR dependencies required
7   unknown dependencies
8   build failed
9   package install failed
10  partial upgrade completed with failures
```

## First Target Packages

Initial real-world packages:

```text
visual-studio-code-bin
helium-browser-bin
```

These are useful because they cover binary AUR packages, desktop integration, and update behavior without first needing the full complexity of source-heavy AUR packages.
