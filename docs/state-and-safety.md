# State and Safety Model

## Trust Boundary

`aur-step` has two trust zones:

```text
Root supervisor zone:
  trusted code owned by root
  pacman operations
  state database

Build user zone:
  AUR git repositories
  PKGBUILD execution
  makepkg output
```

The root supervisor must treat the build user zone as untrusted.

## Suggested State File

Use a root-owned TOML or SQLite state store.

TOML is enough for the first version:

```toml
[packages.visual-studio-code-bin]
aur_name = "visual-studio-code-bin"
repo_url = "https://aur.archlinux.org/visual-studio-code-bin.git"
build_path = "/home/alice/aurbuild/visual-studio-code-bin"
last_built_version = "1.101.0-1"
last_installed_version = "1.101.0-1"
last_commit = "..."
reviewed_commit = "..."
```

SQLite is better later if upgrade history and structured logs become useful.

## Filesystem Ownership

Root-owned:

```text
/usr/bin/aur-step
/etc/aur-step.toml
/var/lib/aur-step
/var/log/aur-step
```

User-owned:

```text
/home/alice/aurbuild
/home/alice/aurbuild/<pkg>
```

Root should not chown AUR checkouts to root.

## Privilege Dropping

The implementation should not rely on shell quoting for privilege boundaries.

Implementation behavior:

```text
clone3 without CLONE_VM
setgroups
setgid(configured build-user gid)
setuid(configured build-user uid)
exec git/makepkg
```

The parent receives pre-exec failures over a close-on-exec error pipe and waits
for the child directly. Generated files such as `.SRCINFO` are opened by the
child after UID/GID drop so they stay owned by the build user.

## Environment for Builds

Build environment should be minimal:

```text
HOME=<build-user-home>
USER=<build-user>
LOGNAME=<build-user>
PATH=/usr/local/bin:/usr/bin:/bin/<build-user-home>/.local/bin
GIT_TERMINAL_PROMPT=0
GIT_ASKPASS=/usr/bin/false
LANG from host if safe
LC_* from host if safe
```

Do not forward:

```text
SSH_AUTH_SOCK
GPG_AGENT_INFO
GNUPGHOME
DOCKER_HOST
KUBECONFIG
AWS_*
GOOGLE_*
NPM_TOKEN
NODE_AUTH_TOKEN
any token/password/secret variable
```

Standard input is `/dev/null` for build-user commands. This prevents AUR code
and Git credential prompts from consuming input intended for the root
supervisor.

## Dependency Safety

Official repo dependencies are installed by root with `pacman`.

AUR dependencies are not silently trusted unless policy allows it.

Default policy:

```text
If AUR dependency is found, report and stop.
Codex can then explicitly install the AUR dependency first.
```

Optional later policy:

```text
--auto-aur-deps
```

Even with `--auto-aur-deps`, each new AUR package should have an inspectable plan.

## Update Safety

`aur-step upgrade` should avoid partial hidden behavior.

Good behavior:

```text
show plan
build as many as possible
install successful builds
report failures clearly
never leave root-owned files in AUR build trees
```

Bad behavior:

```text
silently skip failed packages
run makepkg as root
let makepkg call sudo
continue after repo dependency resolution is ambiguous
```

## Review State

`reviewed_commit` is the durable trust marker for a package checkout.

Allowed:

```text
record the current git commit after human/AI review
show reviewed=true only when current commit equals reviewed_commit
mark upgrade packages ready_for_build only when upgrade_available and reviewed
require a new review after git pull advances the checkout
```

Not allowed:

```text
treat an imported Yay checkout as reviewed by default
carry review forward across commit changes
```

## Clean Build Option

Implemented command:

```bash
aur-step clean <pkg>
```

This removes only:

```text
/home/alice/aurbuild/<pkg>/src
/home/alice/aurbuild/<pkg>/pkg
package artifacts for that package
```

It should not remove:

```text
PKGBUILD
.SRCINFO
.git
signature files
aur-step state
other package build directories
```

## Interaction With dev-sandbox

`aur-step` and the local dev-sandbox solve different problems.

`aur-step`:

```text
supervises AUR package build/install privilege boundaries
```

dev-sandbox:

```text
limits supply-chain behavior of package managers inside development projects
```

Do not mount sensitive directories into AUR build environments just to satisfy package build scripts.

## Interaction With Yay

Yay compatibility is an import and migration feature.

Allowed:

```text
read Yay's effective config
read package checkouts under the configured build user's Yay cache
reuse an existing Yay checkout as the initial build path
record imported installed versions in /var/lib/aur-step
```

Not allowed:

```text
run yay as root
let yay perform installs for aur-step
trust a Yay cache checkout as reviewed only because it exists
silently install cache-only packages that are not present in pacman -Qm
```

## Plan Mode

Commands that would mutate the system package database should expose a plan mode.

Implemented:

```bash
aur-step install-repo-deps --plan <pkg>
aur-step install-built --plan <pkg>
aur-step remove --plan <pkg>...
```

Plan mode may read package metadata and discover local package artifacts, but it
must not call `pacman -S`, `pacman -U`, or `pacman -R`.

`upgrade --plan --refresh` is a metadata-mutating plan mode: it may run
`git pull --ff-only` and `makepkg --printsrcinfo` as the build user, but it must
not run package build functions or install artifacts.

## Filesystem and Artifact Checks

Configuration and root state are opened without following symlinks. When
running as root, their directory chain and files must be root-owned and not
group/other-writable; root-owned sticky directories such as `/tmp` are accepted
for isolated integration tests.

Build directories are traversed with directory descriptors and `O_NOFOLLOW`
before ownership or mode changes. Package records may point only to a direct
child of `build_root` or `yay_build_dir`.

Before `pacman -U`, each package archive must be a regular, single-link file
owned by the configured build user. It is copied through a no-follow descriptor
into a root-owned mode-0700 staging directory beside the state database.
`pacman -Qp` must parse the staged copy before installation. Staging is removed
after the install attempt.

These controls protect the root boundary from path substitution. They do not
sandbox `PKGBUILD` execution: build code retains the build user's normal
filesystem and network access.

Upgrade plan output must keep blocked packages explicit. If a package has a new
available version but the checkout is not reviewed, it should report
`ready_for_build=false`, no `planned_actions`, and a `build_blocked_reasons`
entry rather than silently skipping or building it.
