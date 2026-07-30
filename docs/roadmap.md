# Implementation Roadmap

## Phase 1: Safe Mechanical Steps

- Implement `import-yay`:
  - read `pacman -Qm`,
  - match installed foreign packages to `~/.cache/yay/<pkg>` checkouts,
  - import installed versions and current checkout commits into SQLite,
  - report foreign packages missing Yay cache and cache-only Yay dirs.
- Implement `fetch`:
  - clone missing AUR repo as build user,
  - `git pull --ff-only` existing repo as build user,
  - record last commit in SQLite.
- Implement `inspect`:
  - print PKGBUILD path,
  - print `.SRCINFO` path,
  - print latest commit,
  - print diff since reviewed commit if known.
- Implement `.SRCINFO` refresh:
  - run `makepkg --printsrcinfo` as build user when `.SRCINFO` is absent.

Status: implemented.

Review persistence status: `aur-step review <pkg>` records the current git
commit as reviewed, and `inspect`/`upgrade --plan` expose whether the current
commit matches `reviewed_commit`.

## Phase 2: Dependency Resolution

- Expand `.SRCINFO` parser for arch-specific fields.
- Use `pacman -T` for missing dependency checks.
- Use `pacman -Si` for official repo classification.
- Resolve missing non-repo dependencies against the configured AUR URL.
- Emit confirmed AUR dependencies as `aur_deps` and unresolved entries as
  `unknown_deps`.
- Implement `install-repo-deps` with `pacman -S --needed`.

Status: implemented for direct and arch-specific dependencies. Recursive AUR
dependency planning is implemented as a read-only walk over existing local
`.SRCINFO` files; missing AUR checkouts are reported as `needs_fetch`.

Current provider status: local sync-repo `Provides` candidates are reported, but
provider selection is explicit through `install-repo-deps --provider
dependency=package`.

## Phase 3: Build and Install

- Implement `build`:
  - run `makepkg --noconfirm --needed` as build user,
  - prevent `makepkg` from invoking sudo,
  - collect built package artifacts.
- Implement `install-built`:
  - install artifacts with `pacman -U --needed`,
  - update SQLite state after success.
- Implement `clean`:
  - remove `src/`, `pkg/`, and package artifacts,
  - preserve reviewable source files, git metadata, signatures, and state.

Status: implemented with `makepkg --noconfirm`; dependency installation is kept
outside makepkg.

## Phase 4: High-Level Install

- Implement `install <pkg>...` as:
  - fetch,
  - inspect/review gate,
  - deps,
  - install repo deps,
  - build,
  - install built.
- Stop on AUR dependencies unless `--auto-aur-deps` exists and is enabled.

Status: implemented with `--assume-reviewed` as the explicit review gate.
`--provider dependency=package` is threaded into high-level install.
`--auto-aur-deps` recursively installs confirmed AUR dependencies while
preserving the review gate for each package.

## Phase 4.5: Remove

- Implement `remove --plan <pkg>...`:
  - show pacman removal arguments,
  - show whether packages are managed in state,
  - avoid mutating package database or state.
- Implement `remove <pkg>...`:
  - run `pacman -Rns --noconfirm` as root,
  - remove package records from state only after pacman succeeds.

Status: implemented.

## Phase 5: Upgrade

- Implement `upgrade --plan`:
  - `pacman -Qm`,
  - SQLite managed packages,
  - AUR checkout update,
  - installed vs available version compare.
- Implement `upgrade`:
  - run `pacman -Syu`,
  - build changed AUR packages,
  - install successful artifacts,
  - report partial failures.

Status: `upgrade --plan` is implemented as a read-only planner from existing
state and existing `.SRCINFO` files. `upgrade --plan --refresh` refreshes
existing git checkouts and `.SRCINFO` as the build user. The plan reports
`ready_for_build`, `planned_actions`, and `build_blocked_reasons`, with upgrade
actions gated on `reviewed_commit`. Non-plan `upgrade` runs `pacman -Syu`, then
executes only reviewed ready packages through dependency install, build, and
artifact install. Explicit provider selections are threaded into high-level
upgrade execution. Current packages are reported as no-action rather than
failures; blocked or failed packages make the command fail.

## Phase 6: Hardening

- Build-user commands receive `/dev/null` as stdin and disable interactive Git
  credential prompts.
- Config, state, build directories, and package artifacts use no-follow checks.
- Artifacts are ownership/link validated, staged under root ownership, and
  parsed by `pacman -Qp` before installation.
- Root subprocesses use fixed `/usr/bin` paths and review diffs disable external
  diff/text-conversion drivers.
- Expand the environment allowlist for build commands if specific safe variables are needed.

Current root integration coverage includes clone3 fetch, metadata refresh, and
`makepkg --noconfirm` build as the configured build user, plus `pacman -U`
artifact installation with state update. These tests are disabled by default,
including when `cargo test` runs as root. Set
`AUR_STEP_RUN_ROOT_INTEGRATION=1` and optionally
`AUR_STEP_ROOT_TEST_USER=<non-root-user>` only in an isolated Arch test
environment.

Current JSON contract coverage is documented in `docs/json-contracts.md`.
Current command examples are documented in `docs/usage-examples.md`.
