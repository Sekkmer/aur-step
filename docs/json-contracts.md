# JSON Output Contracts

`aur-step --json` prints one JSON document per command invocation.

Unless noted otherwise:

- Paths are UTF-8 strings.
- Package names are validated before use.
- Plan commands do not mutate the system package database.
- Error exits print diagnostics on stderr, not a JSON error envelope.

## Dependency Plan

Commands:

```bash
aur-step deps <pkg> --json
aur-step install-repo-deps --plan <pkg> --json
```

Important fields:

```json
{
  "package": "example",
  "version": "1.0-1",
  "repo_deps_installed": ["glibc"],
  "repo_deps_missing": ["libxkbfile"],
  "provider_deps": [
    {
      "dependency": "ttf-font",
      "candidates": ["noto-fonts"]
    }
  ],
  "aur_deps": ["example-aur-dep"],
  "unknown_deps": [],
  "aur_or_unknown_deps": [],
  "optdepends": []
}
```

`provider_deps`, `aur_deps`, and `unknown_deps` are stop points for automatic
repo dependency installation unless the command explicitly supports resolving
that category.

## Recursive Dependencies

Command:

```bash
aur-step deps --recursive <pkg> --json
```

Package statuses:

- `inspected`: local `.SRCINFO` exists and was classified.
- `needs_fetch`: confirmed AUR dependency exists, but local `.SRCINFO` is absent.
- `already_seen`: dependency cycle or duplicate was detected.
- `error`: local dependency metadata could not be read or parsed.

## Review State

Command:

```bash
aur-step inspect <pkg> --json
```

`reviewed` is true only when the current git commit equals `reviewed_commit`.

When a reviewed commit exists, `review_diff` is present:

```json
{
  "status": "changed",
  "changed_files": [
    {
      "status": "M",
      "path": "PKGBUILD",
      "previous_path": null
    }
  ],
  "stat": "...",
  "diff": "..."
}
```

Review diff statuses:

- `current`
- `changed`
- `missing_current_commit`
- `error`

## Upgrade Plan

Command:

```bash
aur-step upgrade --plan --json
```

Package statuses:

- `current`
- `upgrade_available`
- `newer_than_srcinfo`
- `missing_srcinfo`
- `not_installed`
- `version_unknown`
- `error`

Action fields:

- `reviewed`: current checkout commit equals `reviewed_commit`.
- `ready_for_build`: package is upgradeable and reviewed.
- `planned_actions`: future low-level action sequence for ready packages.
- `build_blocked_reasons`: explicit blockers for non-ready upgrade packages.

## Upgrade Execution

Command:

```bash
aur-step upgrade --json
```

Package result labels:

- `completed`: dependency install, build, and artifact install all succeeded.
- `no_action`: package is current or installed version is newer than `.SRCINFO`.
- `blocked`: package needs action but is not safe to build.
- `failed`: a step failed after execution began.

Top-level counts:

```json
{
  "completed_count": 1,
  "no_action_count": 3,
  "blocked_count": 0,
  "failed_count": 0
}
```

The command exits successfully when packages are completed or no-action. It exits
with failure when any package is blocked or failed.

## Install Execution

Command:

```bash
aur-step install --json <pkg>
```

The top-level `packages` array is emitted in dependency order. Each package entry
contains:

- `fetch`
- `dependency_plan`
- `repo_dependency_install`
- `build`
- `install`

`install --auto-aur-deps` recursively installs confirmed AUR dependencies before
the package that requires them. Review gates still apply unless
`--assume-reviewed` is set.

## Remove Plan

Command:

```bash
aur-step remove --plan <pkg>... --json
```

The plan includes `pacman_args`, whether each package is managed by `aur-step`,
and an empty `state_removed` array. Non-plan remove fills `state_removed` only
after successful `pacman -Rns`.
