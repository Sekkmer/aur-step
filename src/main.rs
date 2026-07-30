mod cli;
mod config;
mod db;
mod deps;
mod exec;
mod fs_safety;
mod model;
mod srcinfo;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};
use config::Config;
use db::Database;

fn main() {
    if let Err(err) = run() {
        eprintln!("aur-step: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    let db = Database::open(&config)?;

    match cli.command {
        Command::Init => commands::init(&config, &db, cli.json),
        Command::Status => commands::status(&config, &db, cli.json),
        Command::Fetch { package } => commands::fetch(&config, &db, &package, cli.json),
        Command::ImportYay { include_cache_only } => {
            commands::import_yay(&config, &db, include_cache_only, cli.json)
        }
        Command::Inspect { package } => commands::inspect(&config, &db, &package, cli.json),
        Command::Review { package } => commands::review(&config, &db, &package, cli.json),
        Command::Deps { recursive, package } => {
            commands::deps(&config, &db, &package, recursive, cli.json)
        }
        Command::InstallRepoDeps {
            plan,
            providers,
            package,
        } => commands::install_repo_deps(&config, &db, &package, plan, &providers, cli.json),
        Command::Build { package } => commands::build(&config, &db, &package, cli.json),
        Command::InstallBuilt { plan, package } => {
            commands::install_built(&config, &db, &package, plan, cli.json)
        }
        Command::Clean { package } => commands::clean(&config, &db, &package, cli.json),
        Command::Remove { plan, packages } => commands::remove(&db, &packages, plan, cli.json),
        Command::Install {
            assume_reviewed,
            auto_aur_deps,
            providers,
            packages,
        } => commands::install(
            &config,
            &db,
            &packages,
            assume_reviewed,
            auto_aur_deps,
            &providers,
            cli.json,
        ),
        Command::Upgrade {
            plan,
            refresh,
            providers,
        } => commands::upgrade(&config, &db, plan, refresh, &providers, cli.json),
    }
}

mod commands {
    use super::{deps, exec, fs_safety, srcinfo, Config, Database};
    use anyhow::{bail, Context, Result};
    use camino::{Utf8Path, Utf8PathBuf};
    use serde::Serialize;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs::{self, File, OpenOptions};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::process::{Command, Stdio};

    pub fn init(config: &Config, db: &Database, json: bool) -> Result<()> {
        exec::require_root()?;
        let user = exec::lookup_build_user(&config.build_user)?;
        db.migrate()?;
        exec::ensure_user_owned_dir(&config.build_root, &user)?;
        emit(
            json,
            "initialized",
            InitOutput {
                build_user: &config.build_user,
                build_root: config.build_root.as_str(),
                state_db: config.state_db.as_str(),
            },
        )
    }

    pub fn status(config: &Config, db: &Database, json: bool) -> Result<()> {
        let packages = db.list_packages()?;
        emit(
            json,
            "status",
            StatusOutput {
                build_user: &config.build_user,
                package_count: packages.len(),
                packages,
            },
        )
    }

    pub fn fetch(config: &Config, db: &Database, package: &str, json: bool) -> Result<()> {
        let output = fetch_inner(config, db, package)?;
        emit(json, "fetched", output)
    }

    fn fetch_inner(config: &Config, db: &Database, package: &str) -> Result<FetchOutput> {
        exec::require_root()?;
        validate_package_name(package)?;
        let user = exec::lookup_build_user(&config.build_user)?;
        exec::ensure_user_owned_dir(&config.build_root, &user)?;
        let path = package_path(config, db, package)?;
        let repo_url = aur_repo_url(config, package);
        if path.join(".git").exists() {
            let branch = git_current_branch(&user, &path)?;
            exec::run_as_user(exec::UserCommand {
                user: &user,
                program: "git",
                args: vec![
                    "pull".to_owned(),
                    "--ff-only".to_owned(),
                    "origin".to_owned(),
                    branch,
                ],
                cwd: &path,
                stdout_file: Some(&path.join(".aur-step-fetch.log")),
                stderr_file: Some(&path.join(".aur-step-fetch.log")),
            })?;
        } else {
            exec::run_as_user(exec::UserCommand {
                user: &user,
                program: "git",
                args: vec!["clone".to_owned(), repo_url.clone(), path.to_string()],
                cwd: &config.build_root,
                stdout_file: None,
                stderr_file: Some(&config.build_root.join(format!("{package}.fetch.log"))),
            })?;
        }
        refresh_srcinfo(config, &user, &path)?;
        let last_commit = git_head(&user, &path)?;
        db.upsert_package_stub(package, path.as_str())?;
        db.update_last_commit(package, last_commit.as_deref())?;
        Ok(FetchOutput {
            package: package.to_owned(),
            repo_url,
            path,
            last_commit,
        })
    }

    pub fn import_yay(
        config: &Config,
        db: &Database,
        include_cache_only: bool,
        json: bool,
    ) -> Result<()> {
        let user = exec::lookup_build_user(&config.build_user)?;
        let installed = foreign_packages()?;
        let yay_dirs = yay_package_dirs(&config.yay_build_dir)?;
        let installed_names = installed.keys().cloned().collect::<BTreeSet<_>>();

        let mut imported = Vec::new();
        let mut missing_yay_cache = Vec::new();
        for (package, version) in &installed {
            let build_path = yay_dirs
                .get(package)
                .cloned()
                .unwrap_or_else(|| config.package_dir(package));
            if !yay_dirs.contains_key(package) {
                missing_yay_cache.push(package.clone());
            }
            let repo_url = aur_repo_url(config, package);
            let last_commit = git_head(&user, &build_path)?;
            db.upsert_imported_package(
                package,
                &repo_url,
                build_path.as_str(),
                version,
                last_commit.as_deref(),
            )?;
            imported.push(YayImportPackage {
                package: package.clone(),
                installed_version: version.clone(),
                build_path,
                has_yay_cache: yay_dirs.contains_key(package),
                last_commit,
            });
        }

        let cache_only = yay_dirs
            .keys()
            .filter(|package| !installed_names.contains(*package))
            .cloned()
            .collect::<Vec<_>>();

        if include_cache_only {
            for package in &cache_only {
                let build_path = yay_dirs
                    .get(package)
                    .expect("cache_only package came from yay_dirs");
                let repo_url = aur_repo_url(config, package);
                let last_commit = git_head(&user, build_path)?;
                db.upsert_imported_package(
                    package,
                    &repo_url,
                    build_path.as_str(),
                    "",
                    last_commit.as_deref(),
                )?;
            }
        }

        emit(
            json,
            "yay state imported",
            YayImportOutput {
                yay_build_dir: config.yay_build_dir.clone(),
                imported_count: imported.len(),
                imported,
                missing_yay_cache,
                cache_only,
                cache_only_imported: include_cache_only,
            },
        )
    }

    pub fn inspect(config: &Config, db: &Database, package: &str, json: bool) -> Result<()> {
        validate_package_name(package)?;
        let user = exec::lookup_build_user(&config.build_user)?;
        let path = package_path(config, db, package)?;
        let record = db.get_package(package)?;
        let pkgbuild_path = path.join("PKGBUILD");
        let srcinfo_path = path.join(".SRCINFO");
        let last_commit = git_head(&user, &path)?;
        let reviewed_commit = record
            .as_ref()
            .and_then(|record| record.reviewed_commit.as_deref());
        let reviewed = is_reviewed_commit(reviewed_commit, last_commit.as_deref());
        let review_diff =
            inspect_review_diff(&user, &path, reviewed_commit, last_commit.as_deref());
        emit(
            json,
            "inspection",
            InspectOutput {
                package,
                path: path.clone(),
                pkgbuild_path: pkgbuild_path.clone(),
                srcinfo_path: srcinfo_path.clone(),
                pkgbuild_exists: pkgbuild_path.exists(),
                srcinfo_exists: srcinfo_path.exists(),
                last_commit,
                reviewed,
                review_diff,
                state_record: record,
            },
        )
    }

    pub fn review(config: &Config, db: &Database, package: &str, json: bool) -> Result<()> {
        validate_package_name(package)?;
        let user = exec::lookup_build_user(&config.build_user)?;
        let path = package_path(config, db, package)?;
        if db.get_package(package)?.is_none() {
            db.upsert_package_stub(package, path.as_str())?;
        }
        let Some(commit) = git_head(&user, &path)? else {
            bail!("{path} is not a git checkout; run aur-step fetch {package} first");
        };
        db.update_last_commit(package, Some(&commit))?;
        db.update_reviewed_commit(package, &commit)?;
        emit(
            json,
            "review recorded",
            ReviewOutput {
                package,
                path,
                reviewed_commit: commit,
            },
        )
    }

    pub fn deps(
        config: &Config,
        db: &Database,
        package: &str,
        recursive: bool,
        json: bool,
    ) -> Result<()> {
        validate_package_name(package)?;
        if recursive {
            return emit(
                json,
                "recursive dependencies planned",
                recursive_deps(config, db, package)?,
            );
        }
        let srcinfo_path = package_path(config, db, package)?.join(".SRCINFO");
        if !srcinfo_path.exists() {
            bail!(
                "{} does not exist; run aur-step fetch {} first",
                srcinfo_path,
                package
            );
        }
        let srcinfo = srcinfo::parse_file(&srcinfo_path)
            .with_context(|| format!("failed to parse {}", srcinfo_path))?;
        let mut plan = deps::classify(&srcinfo)?;
        resolve_aur_dependencies(config, &mut plan)?;
        emit(json, "dependencies classified", plan)
    }

    fn recursive_deps(config: &Config, db: &Database, root: &str) -> Result<RecursiveDepsOutput> {
        let mut seen = BTreeSet::new();
        let mut packages = Vec::new();
        collect_recursive_deps(config, db, root, None, &mut seen, &mut packages)?;
        let inspected_count = packages
            .iter()
            .filter(|package| package.status == "inspected")
            .count();
        let needs_fetch_count = packages
            .iter()
            .filter(|package| package.status == "needs_fetch")
            .count();
        let error_count = packages
            .iter()
            .filter(|package| package.status == "error")
            .count();
        Ok(RecursiveDepsOutput {
            root: root.to_owned(),
            inspected_count,
            needs_fetch_count,
            error_count,
            packages,
        })
    }

    fn collect_recursive_deps(
        config: &Config,
        db: &Database,
        package: &str,
        required_by: Option<&str>,
        seen: &mut BTreeSet<String>,
        packages: &mut Vec<RecursiveDepsPackageOutput>,
    ) -> Result<()> {
        validate_package_name(package)?;
        if !seen.insert(package.to_owned()) {
            packages.push(RecursiveDepsPackageOutput {
                package: package.to_owned(),
                required_by: required_by.map(ToOwned::to_owned),
                status: "already_seen",
                build_path: package_path(config, db, package)?,
                srcinfo_path: None,
                dependency_plan: None,
                error: None,
            });
            return Ok(());
        }

        let build_path = package_path(config, db, package)?;
        let srcinfo_path = build_path.join(".SRCINFO");
        if !srcinfo_path.exists() {
            packages.push(RecursiveDepsPackageOutput {
                package: package.to_owned(),
                required_by: required_by.map(ToOwned::to_owned),
                status: "needs_fetch",
                build_path,
                srcinfo_path: Some(srcinfo_path),
                dependency_plan: None,
                error: None,
            });
            return Ok(());
        }

        let srcinfo = match srcinfo::parse_file(&srcinfo_path)
            .with_context(|| format!("failed to parse {}", srcinfo_path))
        {
            Ok(srcinfo) => srcinfo,
            Err(error) => {
                packages.push(RecursiveDepsPackageOutput {
                    package: package.to_owned(),
                    required_by: required_by.map(ToOwned::to_owned),
                    status: "error",
                    build_path,
                    srcinfo_path: Some(srcinfo_path),
                    dependency_plan: None,
                    error: Some(error.to_string()),
                });
                return Ok(());
            }
        };
        let mut plan = deps::classify(&srcinfo)?;
        resolve_aur_dependencies(config, &mut plan)?;
        let aur_deps = plan.aur_deps.clone();
        packages.push(RecursiveDepsPackageOutput {
            package: package.to_owned(),
            required_by: required_by.map(ToOwned::to_owned),
            status: "inspected",
            build_path,
            srcinfo_path: Some(srcinfo_path),
            dependency_plan: Some(plan),
            error: None,
        });
        for aur_dep in aur_deps {
            collect_recursive_deps(config, db, &aur_dep, Some(package), seen, packages)?;
        }
        Ok(())
    }

    pub fn install_repo_deps(
        config: &Config,
        db: &Database,
        package: &str,
        plan_only: bool,
        provider_selection_args: &[String],
        json: bool,
    ) -> Result<()> {
        let output =
            install_repo_deps_inner(config, db, package, plan_only, provider_selection_args)?;
        emit(
            json,
            if plan_only {
                "repo dependency install planned"
            } else {
                "repo dependencies installed"
            },
            output,
        )
    }

    fn install_repo_deps_inner(
        config: &Config,
        db: &Database,
        package: &str,
        plan_only: bool,
        provider_selection_args: &[String],
    ) -> Result<RepoDepsOutput> {
        validate_package_name(package)?;
        let srcinfo_path = package_path(config, db, package)?.join(".SRCINFO");
        if !srcinfo_path.exists() {
            bail!(
                "{} does not exist; run aur-step deps {} first",
                srcinfo_path,
                package
            );
        }
        let srcinfo = srcinfo::parse_file(&srcinfo_path)
            .with_context(|| format!("failed to parse {}", srcinfo_path))?;
        let mut plan = deps::classify(&srcinfo)?;
        resolve_aur_dependencies(config, &mut plan)?;
        let provider_selections = parse_provider_selections(provider_selection_args)?;
        let selected_providers = apply_provider_selections(&mut plan, &provider_selections)?;
        let has_unresolved = !plan.provider_deps.is_empty()
            || !plan.aur_deps.is_empty()
            || !plan.unknown_deps.is_empty();
        if has_unresolved && !plan_only {
            bail!(
                "{} has unresolved provider/AUR/unknown dependencies: {}",
                package,
                unresolved_dependency_summary(&plan)
            );
        }
        let mut pacman_args = vec![
            "-S".to_owned(),
            "--needed".to_owned(),
            "--noconfirm".to_owned(),
        ];
        pacman_args.extend(plan.repo_deps_missing.iter().cloned());
        if !plan_only {
            exec::require_root()?;
        }
        if !plan_only && !plan.repo_deps_missing.is_empty() {
            let mut args = vec![
                "-S".to_owned(),
                "--needed".to_owned(),
                "--noconfirm".to_owned(),
            ];
            args.extend(plan.repo_deps_missing.iter().cloned());
            let status = Command::new("/usr/bin/pacman")
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("failed to run pacman for repo dependencies")?;
            if !status.success() {
                bail!("pacman failed while installing repo dependencies");
            }
        }
        Ok(RepoDepsOutput {
            package: package.to_owned(),
            plan_only,
            pacman_args,
            can_install: !has_unresolved,
            selected_providers,
            installed: plan.repo_deps_missing,
            already_present: plan.repo_deps_installed,
            provider_deps: plan.provider_deps,
            aur_deps: plan.aur_deps,
            unknown_deps: plan.unknown_deps,
            aur_or_unknown_deps: plan.aur_or_unknown_deps,
            srcinfo_path,
        })
    }

    pub fn build(config: &Config, db: &Database, package: &str, json: bool) -> Result<()> {
        let output = build_inner(config, db, package)?;
        emit(json, "built", output)
    }

    fn build_inner(config: &Config, db: &Database, package: &str) -> Result<BuildOutput> {
        exec::require_root()?;
        validate_package_name(package)?;
        let user = exec::lookup_build_user(&config.build_user)?;
        let path = package_path(config, db, package)?;
        if !path.join("PKGBUILD").exists() {
            bail!(
                "{} does not contain PKGBUILD; run aur-step fetch {package} first",
                path
            );
        }
        refresh_srcinfo(config, &user, &path)?;
        exec::run_as_user(exec::UserCommand {
            user: &user,
            program: "makepkg",
            args: vec!["--noconfirm".to_owned()],
            cwd: &path,
            stdout_file: Some(&path.join(".aur-step-makepkg.log")),
            stderr_file: Some(&path.join(".aur-step-makepkg.log")),
        })?;
        let artifacts = package_artifacts(&path, &user)?;
        let version = srcinfo::parse_file(&path.join(".SRCINFO"))?.version();
        db.update_last_built_version(package, version.as_deref())?;
        Ok(BuildOutput {
            package: package.to_owned(),
            path,
            version,
            artifacts,
        })
    }

    pub fn install_built(
        config: &Config,
        db: &Database,
        package: &str,
        plan_only: bool,
        json: bool,
    ) -> Result<()> {
        let output = install_built_inner(config, db, package, plan_only)?;
        emit(
            json,
            if plan_only {
                "built package install planned"
            } else {
                "installed built package"
            },
            output,
        )
    }

    fn install_built_inner(
        config: &Config,
        db: &Database,
        package: &str,
        plan_only: bool,
    ) -> Result<InstallBuiltOutput> {
        validate_package_name(package)?;
        let user = exec::lookup_build_user(&config.build_user)?;
        let path = package_path(config, db, package)?;
        let artifacts = package_artifacts(&path, &user)?;
        if artifacts.is_empty() {
            bail!("no package artifacts found in {path}; run aur-step build {package} first");
        }
        let mut args = vec![
            "-U".to_owned(),
            "--needed".to_owned(),
            "--noconfirm".to_owned(),
        ];
        args.extend(artifacts.iter().map(ToString::to_string));
        if !plan_only {
            exec::require_root()?;
            let (staging, staged_artifacts) =
                stage_package_artifacts(config, &path, &artifacts, &user)?;
            let mut install_args = vec![
                "-U".to_owned(),
                "--needed".to_owned(),
                "--noconfirm".to_owned(),
            ];
            install_args.extend(staged_artifacts.iter().map(ToString::to_string));
            let status = Command::new("/usr/bin/pacman")
                .args(&install_args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("failed to run pacman -U")?;
            drop(staging);
            if !status.success() {
                bail!("pacman -U failed for built package artifacts");
            }
        }
        let version = srcinfo::parse_file(&path.join(".SRCINFO"))
            .ok()
            .and_then(|info| info.version());
        if !plan_only {
            db.update_last_installed_version(package, version.as_deref())?;
        }
        Ok(InstallBuiltOutput {
            package: package.to_owned(),
            plan_only,
            path,
            version,
            pacman_args: args,
            artifacts,
        })
    }

    pub fn clean(config: &Config, db: &Database, package: &str, json: bool) -> Result<()> {
        validate_package_name(package)?;
        let path = package_path(config, db, package)?;
        if !path.exists() {
            bail!("{path} does not exist; nothing to clean");
        }
        let mut removed = Vec::new();
        for generated_dir in ["src", "pkg"] {
            let dir = path.join(generated_dir);
            if dir.exists() {
                fs::remove_dir_all(&dir).with_context(|| format!("failed to remove {dir}"))?;
                removed.push(CleanedPath {
                    path: dir,
                    kind: "directory",
                });
            }
        }
        let user = exec::lookup_build_user(&config.build_user)?;
        for artifact in package_artifacts(&path, &user)? {
            fs::remove_file(&artifact)
                .with_context(|| format!("failed to remove package artifact {artifact}"))?;
            removed.push(CleanedPath {
                path: artifact,
                kind: "package_artifact",
            });
        }
        emit(
            json,
            "cleaned",
            CleanOutput {
                package,
                path,
                removed,
            },
        )
    }

    pub fn remove(db: &Database, packages: &[String], plan_only: bool, json: bool) -> Result<()> {
        if packages.is_empty() {
            bail!("remove needs at least one package");
        }
        for package in packages {
            validate_package_name(package)?;
        }
        let mut package_plans = Vec::new();
        for package in packages {
            package_plans.push(RemovePackageOutput {
                package: package.clone(),
                managed: db.get_package(package)?.is_some(),
            });
        }
        let mut pacman_args = vec!["-Rns".to_owned(), "--noconfirm".to_owned()];
        pacman_args.extend(packages.iter().cloned());
        if !plan_only {
            exec::require_root()?;
            let status = Command::new("/usr/bin/pacman")
                .args(&pacman_args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .context("failed to run pacman -Rns")?;
            if !status.success() {
                bail!("pacman -Rns failed");
            }
            for package in packages {
                db.delete_package(package)?;
            }
        }
        emit(
            json,
            if plan_only {
                "remove planned"
            } else {
                "packages removed"
            },
            RemoveOutput {
                plan_only,
                pacman_args,
                packages: package_plans,
                state_removed: if plan_only {
                    Vec::new()
                } else {
                    packages.to_vec()
                },
            },
        )
    }

    pub fn install(
        config: &Config,
        db: &Database,
        packages: &[String],
        assume_reviewed: bool,
        auto_aur_deps: bool,
        provider_selection_args: &[String],
        json: bool,
    ) -> Result<()> {
        exec::require_root()?;
        if packages.is_empty() {
            bail!("install needs at least one package");
        }
        for package in packages {
            validate_package_name(package)?;
        }
        let provider_selections = parse_provider_selections(provider_selection_args)?;
        let mut state = InstallRunState {
            installing: Vec::new(),
            completed: Vec::new(),
            outputs: Vec::new(),
        };
        for package in packages {
            install_package_recursive(
                config,
                db,
                package,
                None,
                InstallOptions {
                    assume_reviewed,
                    auto_aur_deps,
                    provider_selections: &provider_selections,
                },
                &mut state,
            )?;
        }
        emit(
            json,
            "install complete",
            InstallOutput {
                completed: state.completed,
                assume_reviewed,
                auto_aur_deps,
                packages: state.outputs,
            },
        )
    }

    fn install_package_recursive(
        config: &Config,
        db: &Database,
        package: &str,
        required_by: Option<&str>,
        options: InstallOptions<'_>,
        state: &mut InstallRunState,
    ) -> Result<()> {
        if state.completed.iter().any(|completed| completed == package) {
            return Ok(());
        }
        if state.installing.iter().any(|active| active == package) {
            bail!(
                "recursive AUR dependency cycle detected: {} -> {package}",
                state.installing.join(" -> ")
            );
        }
        state.installing.push(package.to_owned());

        let fetch = fetch_inner(config, db, package)?;
        let mut dependency_plan = dependency_plan_for_package(config, db, package)?;
        let selected_provider_args =
            provider_selection_args_for_plan(&dependency_plan, options.provider_selections);
        let selected_provider_entries =
            provider_selections_for_plan(&dependency_plan, options.provider_selections);
        apply_provider_selections(&mut dependency_plan, &selected_provider_entries)?;

        if !options.assume_reviewed {
            require_reviewed_package(config, db, package)?;
        }

        if !dependency_plan.provider_deps.is_empty()
            || !dependency_plan.unknown_deps.is_empty()
            || (!options.auto_aur_deps && !dependency_plan.aur_deps.is_empty())
        {
            bail!(
                "{} has unresolved provider/AUR/unknown dependencies: {}",
                package,
                unresolved_dependency_summary(&dependency_plan)
            );
        }

        if options.auto_aur_deps {
            let aur_deps = dependency_plan.aur_deps.clone();
            for aur_dep in aur_deps {
                install_package_recursive(config, db, &aur_dep, Some(package), options, state)?;
            }
        }

        let repo_dependency_install =
            install_repo_deps_inner(config, db, package, false, &selected_provider_args)?;
        let build = build_inner(config, db, package)?;
        let install = install_built_inner(config, db, package, false)?;
        state.completed.push(package.to_owned());
        state.outputs.push(InstallPackageOutput {
            package: package.to_owned(),
            required_by: required_by.map(ToOwned::to_owned),
            fetch,
            dependency_plan,
            repo_dependency_install,
            build,
            install,
        });
        state.installing.pop();
        Ok(())
    }

    fn dependency_plan_for_package(
        config: &Config,
        db: &Database,
        package: &str,
    ) -> Result<crate::model::DependencyPlan> {
        let srcinfo_path = package_path(config, db, package)?.join(".SRCINFO");
        if !srcinfo_path.exists() {
            bail!(
                "{} does not exist; run aur-step fetch {} first",
                srcinfo_path,
                package
            );
        }
        let srcinfo = srcinfo::parse_file(&srcinfo_path)
            .with_context(|| format!("failed to parse {}", srcinfo_path))?;
        let mut plan = deps::classify(&srcinfo)?;
        resolve_aur_dependencies(config, &mut plan)?;
        Ok(plan)
    }

    fn require_reviewed_package(config: &Config, db: &Database, package: &str) -> Result<()> {
        let user = exec::lookup_build_user(&config.build_user)?;
        let path = package_path(config, db, package)?;
        let current_commit = git_head(&user, &path)?
            .ok_or_else(|| anyhow::anyhow!("{path} is not a git checkout; cannot verify review"))?;
        let record = db
            .get_package(package)?
            .ok_or_else(|| anyhow::anyhow!("{package} is missing from state"))?;
        if record.reviewed_commit.as_deref() != Some(current_commit.as_str()) {
            bail!(
                "{} fetched and dependency-classified; run `aur-step review {}` after reviewing {} or rerun install with --assume-reviewed",
                package,
                package,
                path.join("PKGBUILD")
            );
        }
        Ok(())
    }

    pub fn upgrade(
        config: &Config,
        db: &Database,
        plan: bool,
        refresh: bool,
        provider_selection_args: &[String],
        json: bool,
    ) -> Result<()> {
        let provider_selections = parse_provider_selections(provider_selection_args)?;
        let build_user = exec::lookup_build_user(&config.build_user)?;
        if refresh || !plan {
            exec::require_root()?;
        }
        if !plan {
            run_repo_upgrade()?;
        }
        let (installed, foreign_packages_error) = match foreign_packages() {
            Ok(packages) => (packages, None),
            Err(error) => (BTreeMap::new(), Some(error.to_string())),
        };
        let packages = db.list_packages()?;
        let mut package_plans = Vec::new();
        for package in packages {
            package_plans.push(plan_upgrade_package(
                &package,
                &installed,
                &build_user,
                refresh || !plan,
            )?);
        }
        let current_count = package_plans
            .iter()
            .filter(|package| package.status == "current")
            .count();
        let upgrade_count = package_plans
            .iter()
            .filter(|package| package.status == "upgrade_available")
            .count();
        let missing_srcinfo_count = package_plans
            .iter()
            .filter(|package| package.status == "missing_srcinfo")
            .count();
        let not_installed_count = package_plans
            .iter()
            .filter(|package| package.status == "not_installed")
            .count();
        let error_count = package_plans
            .iter()
            .filter(|package| package.status == "error")
            .count();
        let ready_for_build_count = package_plans
            .iter()
            .filter(|package| package.ready_for_build)
            .count();
        let build_blocked_count = package_plans
            .iter()
            .filter(|package| !package.build_blocked_reasons.is_empty())
            .count();
        if plan {
            emit(
                json,
                "upgrade planned",
                UpgradeOutput {
                    plan_only: plan,
                    refresh,
                    repo_upgrade_required: true,
                    foreign_packages_error,
                    package_count: package_plans.len(),
                    current_count,
                    upgrade_count,
                    missing_srcinfo_count,
                    not_installed_count,
                    error_count,
                    ready_for_build_count,
                    build_blocked_count,
                    packages: package_plans,
                    note: if refresh {
                        "metadata refresh plan from aur-step state, pacman -Qm, git pull --ff-only, and refreshed .SRCINFO files"
                    } else {
                        "read-only plan from aur-step state, pacman -Qm, and existing .SRCINFO files"
                    },
                },
            )?;
        } else {
            let mut executed = Vec::new();
            for package_plan in package_plans {
                executed.push(execute_upgrade_package(
                    config,
                    db,
                    package_plan,
                    &provider_selections,
                ));
            }
            let completed_count = executed
                .iter()
                .filter(|package| package.result == "completed")
                .count();
            let no_action_count = executed
                .iter()
                .filter(|package| package.result == "no_action")
                .count();
            let blocked_count = executed
                .iter()
                .filter(|package| package.result == "blocked")
                .count();
            let failed_count = executed
                .iter()
                .filter(|package| package.result == "failed")
                .count();
            emit(
                json,
                "upgrade complete",
                UpgradeRunOutput {
                    refresh: true,
                    repo_upgrade_args: vec![
                        "pacman".to_owned(),
                        "-Syu".to_owned(),
                        "--noconfirm".to_owned(),
                    ],
                    foreign_packages_error,
                    package_count: executed.len(),
                    completed_count,
                    no_action_count,
                    blocked_count,
                    failed_count,
                    packages: executed,
                    note: "repo upgrade ran first; AUR packages execute only when upgrade_available and reviewed; provider selections are applied during dependency installation",
                },
            )?;
            if failed_count > 0 || blocked_count > 0 {
                bail!(
                    "upgrade finished with {completed_count} completed, {no_action_count} no action, {blocked_count} blocked, {failed_count} failed"
                );
            }
        }
        Ok(())
    }

    fn execute_upgrade_package(
        config: &Config,
        db: &Database,
        plan: UpgradePackageOutput,
        provider_selections: &[ProviderSelection],
    ) -> UpgradeRunPackageOutput {
        let mut output = UpgradeRunPackageOutput {
            package: plan.package.clone(),
            status: plan.status,
            reviewed: plan.reviewed,
            ready_for_build: plan.ready_for_build,
            planned_actions: plan.planned_actions.clone(),
            build_blocked_reasons: plan.build_blocked_reasons.clone(),
            dependency_install: None,
            build: None,
            install: None,
            result: "pending",
            error: None,
        };

        if !plan.ready_for_build {
            output.result = if plan.status == "current" || plan.status == "newer_than_srcinfo" {
                "no_action"
            } else {
                "blocked"
            };
            return output;
        }

        let provider_args =
            match dependency_plan_for_package(config, db, &plan.package).map(|dependency_plan| {
                provider_selection_args_for_plan(&dependency_plan, provider_selections)
            }) {
                Ok(provider_args) => provider_args,
                Err(error) => {
                    output.result = "failed";
                    output.error = Some(error.to_string());
                    return output;
                }
            };
        let dependency_install =
            match install_repo_deps_inner(config, db, &plan.package, false, &provider_args) {
                Ok(dependency_install) => dependency_install,
                Err(error) => {
                    output.result = "failed";
                    output.error = Some(error.to_string());
                    return output;
                }
            };
        output.dependency_install = Some(dependency_install);

        let build = match build_inner(config, db, &plan.package) {
            Ok(build) => build,
            Err(error) => {
                output.result = "failed";
                output.error = Some(error.to_string());
                return output;
            }
        };
        output.build = Some(build);

        let install = match install_built_inner(config, db, &plan.package, false) {
            Ok(install) => install,
            Err(error) => {
                output.result = "failed";
                output.error = Some(error.to_string());
                return output;
            }
        };
        output.install = Some(install);
        output.result = "completed";
        output
    }

    fn emit<T>(json: bool, text: &str, value: T) -> Result<()>
    where
        T: Serialize,
    {
        if json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            println!("{text}");
        }
        Ok(())
    }

    fn foreign_packages() -> Result<BTreeMap<String, String>> {
        let output = Command::new("/usr/bin/pacman")
            .arg("-Qm")
            .output()
            .context("failed to run pacman -Qm")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("pacman -Qm failed: {}", stderr.trim());
        }

        let text = String::from_utf8(output.stdout).context("pacman -Qm output was not UTF-8")?;
        let mut packages = BTreeMap::new();
        for line in text.lines() {
            let Some((name, version)) = line.split_once(' ') else {
                continue;
            };
            packages.insert(name.to_owned(), version.to_owned());
        }
        Ok(packages)
    }

    fn plan_upgrade_package(
        package: &crate::model::PackageRecord,
        installed: &BTreeMap<String, String>,
        build_user: &exec::BuildUser,
        refresh: bool,
    ) -> Result<UpgradePackageOutput> {
        let build_path = Utf8PathBuf::from_path_buf(package.build_path.clone().into())
            .map_err(|path| anyhow::anyhow!("state path is not UTF-8: {path:?}"))?;
        let srcinfo_path = build_path.join(".SRCINFO");
        let old_commit = git_head(build_user, &build_path).ok().flatten();
        let mut new_commit = old_commit.clone();
        let mut metadata_refreshed = false;
        let mut refresh_error = None;
        if refresh {
            match refresh_upgrade_metadata(build_user, &build_path) {
                Ok(()) => {
                    metadata_refreshed = true;
                    new_commit = git_head(build_user, &build_path).ok().flatten();
                }
                Err(error) => refresh_error = Some(error.to_string()),
            }
        }
        let installed_version = installed
            .get(&package.aur_name)
            .cloned()
            .or_else(|| package.last_installed_version.clone())
            .filter(|version| !version.is_empty());
        let make_output = |status: &'static str,
                           installed_version: Option<String>,
                           available_version: Option<String>,
                           comparison: Option<i32>,
                           error: Option<String>| {
            let reviewed =
                is_reviewed_commit(package.reviewed_commit.as_deref(), new_commit.as_deref());
            let (ready_for_build, planned_actions, build_blocked_reasons) =
                upgrade_build_plan(status, reviewed, refresh_error.as_deref(), error.as_deref());
            UpgradePackageOutput {
                package: package.aur_name.clone(),
                status,
                installed_version,
                available_version,
                comparison,
                build_path: build_path.clone(),
                srcinfo_path: srcinfo_path.clone(),
                last_commit: package.last_commit.clone(),
                reviewed,
                ready_for_build,
                planned_actions,
                build_blocked_reasons,
                old_commit: old_commit.clone(),
                new_commit: new_commit.clone(),
                metadata_refreshed,
                refresh_error: refresh_error.clone(),
                error,
            }
        };

        if installed_version.is_none() {
            return Ok(make_output(
                "not_installed",
                installed_version,
                None,
                None,
                None,
            ));
        }

        if !srcinfo_path.exists() {
            return Ok(make_output(
                "missing_srcinfo",
                installed_version,
                None,
                None,
                None,
            ));
        }

        let srcinfo = match srcinfo::parse_file(&srcinfo_path) {
            Ok(srcinfo) => srcinfo,
            Err(error) => {
                return Ok(make_output(
                    "error",
                    installed_version,
                    None,
                    None,
                    Some(error.to_string()),
                ));
            }
        };
        let available_version = srcinfo.version();
        let Some(installed_version_text) = installed_version.as_deref() else {
            unreachable!("installed_version was checked above");
        };
        let Some(available_version_text) = available_version.as_deref() else {
            return Ok(make_output(
                "version_unknown",
                installed_version,
                available_version,
                None,
                Some(".SRCINFO has no pkgver/pkgrel".to_owned()),
            ));
        };
        let comparison = vercmp(installed_version_text, available_version_text)?;
        let status = upgrade_status_from_comparison(comparison);
        Ok(make_output(
            status,
            installed_version,
            available_version,
            Some(comparison),
            None,
        ))
    }

    fn refresh_upgrade_metadata(user: &exec::BuildUser, build_path: &Utf8Path) -> Result<()> {
        if !build_path.join(".git").exists() {
            bail!("{build_path} is not a git checkout");
        }
        let branch = git_current_branch(user, build_path)?;
        exec::run_as_user(exec::UserCommand {
            user,
            program: "git",
            args: vec![
                "pull".to_owned(),
                "--ff-only".to_owned(),
                "origin".to_owned(),
                branch,
            ],
            cwd: build_path,
            stdout_file: Some(&build_path.join(".aur-step-refresh.log")),
            stderr_file: Some(&build_path.join(".aur-step-refresh.log")),
        })?;
        refresh_srcinfo_from_pkgbuild(user, build_path)?;
        Ok(())
    }

    fn run_repo_upgrade() -> Result<()> {
        exec::require_root()?;
        let status = Command::new("/usr/bin/pacman")
            .args(["-Syu", "--noconfirm"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("failed to run pacman -Syu")?;
        if !status.success() {
            bail!("pacman -Syu failed");
        }
        Ok(())
    }

    fn vercmp(installed: &str, available: &str) -> Result<i32> {
        let output = Command::new("/usr/bin/vercmp")
            .args([installed, available])
            .output()
            .context("failed to run vercmp")?;
        if !output.status.success() {
            bail!("vercmp failed for {installed} and {available}");
        }
        let text = String::from_utf8(output.stdout).context("vercmp output was not UTF-8")?;
        text.trim()
            .parse::<i32>()
            .with_context(|| format!("failed to parse vercmp output: {text:?}"))
    }

    fn upgrade_status_from_comparison(comparison: i32) -> &'static str {
        match comparison.cmp(&0) {
            std::cmp::Ordering::Less => "upgrade_available",
            std::cmp::Ordering::Equal => "current",
            std::cmp::Ordering::Greater => "newer_than_srcinfo",
        }
    }

    fn is_reviewed_commit(reviewed_commit: Option<&str>, current_commit: Option<&str>) -> bool {
        reviewed_commit.is_some() && reviewed_commit == current_commit
    }

    fn resolve_aur_dependencies(
        config: &Config,
        plan: &mut crate::model::DependencyPlan,
    ) -> Result<()> {
        let unresolved = std::mem::take(&mut plan.aur_or_unknown_deps);
        for dep in unresolved {
            if validate_package_name(&dep).is_err() {
                plan.unknown_deps.push(dep.clone());
                plan.aur_or_unknown_deps.push(dep);
                continue;
            }
            if aur_package_exists(config, &dep)? {
                plan.aur_deps.push(dep);
            } else {
                plan.unknown_deps.push(dep.clone());
                plan.aur_or_unknown_deps.push(dep);
            }
        }
        plan.aur_deps.sort();
        plan.aur_deps.dedup();
        plan.unknown_deps.sort();
        plan.unknown_deps.dedup();
        plan.aur_or_unknown_deps.sort();
        plan.aur_or_unknown_deps.dedup();
        Ok(())
    }

    fn aur_package_exists(config: &Config, package: &str) -> Result<bool> {
        let repo_url = aur_repo_url(config, package);
        if let Some(path) = repo_url.strip_prefix("file://") {
            return Ok(std::path::Path::new(path).exists());
        }
        let status = Command::new("/usr/bin/git")
            .args(["ls-remote", "--exit-code", &repo_url, "HEAD"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", "/nonexistent")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .stdin(Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .with_context(|| format!("failed to check AUR repo {repo_url}"))?;
        Ok(status.success())
    }

    fn parse_provider_selections(args: &[String]) -> Result<Vec<ProviderSelection>> {
        let mut selections = Vec::new();
        for arg in args {
            let Some((dependency, package)) = arg.split_once('=') else {
                bail!("provider selection must use dependency=package syntax: {arg}");
            };
            let dependency = dependency.trim();
            let package = package.trim();
            if dependency.is_empty() || package.is_empty() {
                bail!("provider selection must use dependency=package syntax: {arg}");
            }
            validate_package_name(dependency)?;
            validate_package_name(package)?;
            selections.push(ProviderSelection {
                dependency: dependency.to_owned(),
                package: package.to_owned(),
            });
        }
        Ok(selections)
    }

    fn apply_provider_selections(
        plan: &mut crate::model::DependencyPlan,
        selections: &[ProviderSelection],
    ) -> Result<Vec<ProviderSelection>> {
        let mut selected = Vec::new();
        for selection in selections {
            let Some(index) = plan
                .provider_deps
                .iter()
                .position(|provider| provider.dependency == selection.dependency)
            else {
                bail!(
                    "provider selection {}={} does not match a pending provider dependency",
                    selection.dependency,
                    selection.package
                );
            };
            if !plan.provider_deps[index]
                .candidates
                .iter()
                .any(|candidate| candidate == &selection.package)
            {
                bail!(
                    "{} is not a candidate provider for {}; candidates: {}",
                    selection.package,
                    selection.dependency,
                    plan.provider_deps[index].candidates.join(", ")
                );
            }
            let provider = plan.provider_deps.remove(index);
            if !plan.repo_deps_installed.contains(&selection.package)
                && !plan.repo_deps_missing.contains(&selection.package)
            {
                plan.repo_deps_missing.push(selection.package.clone());
            }
            selected.push(ProviderSelection {
                dependency: provider.dependency,
                package: selection.package.clone(),
            });
        }
        plan.repo_deps_missing.sort();
        plan.repo_deps_missing.dedup();
        Ok(selected)
    }

    fn provider_selections_for_plan(
        plan: &crate::model::DependencyPlan,
        selections: &[ProviderSelection],
    ) -> Vec<ProviderSelection> {
        selections
            .iter()
            .filter(|selection| {
                plan.provider_deps
                    .iter()
                    .any(|provider| provider.dependency == selection.dependency)
            })
            .cloned()
            .collect()
    }

    fn provider_selection_args_for_plan(
        plan: &crate::model::DependencyPlan,
        selections: &[ProviderSelection],
    ) -> Vec<String> {
        provider_selections_for_plan(plan, selections)
            .into_iter()
            .map(|selection| format!("{}={}", selection.dependency, selection.package))
            .collect()
    }

    fn inspect_review_diff(
        user: &exec::BuildUser,
        path: &Utf8Path,
        reviewed_commit: Option<&str>,
        current_commit: Option<&str>,
    ) -> Option<ReviewDiffOutput> {
        let reviewed_commit = reviewed_commit?;
        let Some(current_commit) = current_commit else {
            return Some(ReviewDiffOutput {
                reviewed_commit: reviewed_commit.to_owned(),
                current_commit: None,
                status: "missing_current_commit",
                changed_files: Vec::new(),
                stat: None,
                diff: None,
                error: Some("package path is not a git checkout".to_owned()),
            });
        };
        if reviewed_commit == current_commit {
            return Some(ReviewDiffOutput {
                reviewed_commit: reviewed_commit.to_owned(),
                current_commit: Some(current_commit.to_owned()),
                status: "current",
                changed_files: Vec::new(),
                stat: Some(String::new()),
                diff: Some(String::new()),
                error: None,
            });
        }

        match git_review_diff(user, path, reviewed_commit, current_commit) {
            Ok((changed_files, stat, diff)) => Some(ReviewDiffOutput {
                reviewed_commit: reviewed_commit.to_owned(),
                current_commit: Some(current_commit.to_owned()),
                status: "changed",
                changed_files,
                stat: Some(stat),
                diff: Some(diff),
                error: None,
            }),
            Err(error) => Some(ReviewDiffOutput {
                reviewed_commit: reviewed_commit.to_owned(),
                current_commit: Some(current_commit.to_owned()),
                status: "error",
                changed_files: Vec::new(),
                stat: None,
                diff: None,
                error: Some(error.to_string()),
            }),
        }
    }

    fn git_review_diff(
        user: &exec::BuildUser,
        path: &Utf8Path,
        reviewed_commit: &str,
        current_commit: &str,
    ) -> Result<(Vec<GitChangedFile>, String, String)> {
        let range = format!("{reviewed_commit}..{current_commit}");
        let name_status = git_output(
            user,
            path,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--name-status",
                &range,
            ],
        )?;
        let changed_files = parse_git_name_status(&name_status);
        let stat = git_output(
            user,
            path,
            &["diff", "--no-ext-diff", "--no-textconv", "--stat", &range],
        )?;
        let diff = git_output(
            user,
            path,
            &["diff", "--no-ext-diff", "--no-textconv", &range],
        )?;
        Ok((changed_files, stat, diff))
    }

    fn git_output(user: &exec::BuildUser, path: &Utf8Path, args: &[&str]) -> Result<String> {
        exec::run_as_user_capture(
            user,
            "git",
            args.iter().map(|arg| (*arg).to_owned()).collect(),
            path,
        )
        .with_context(|| format!("failed to run git {} in {path}", args.join(" ")))
    }

    fn parse_git_name_status(text: &str) -> Vec<GitChangedFile> {
        text.lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let status = fields.next()?.to_owned();
                let path = fields.next()?.to_owned();
                let previous_path = fields.next().map(ToOwned::to_owned);
                Some(GitChangedFile {
                    status,
                    path,
                    previous_path,
                })
            })
            .collect()
    }

    fn upgrade_build_plan(
        status: &'static str,
        reviewed: bool,
        refresh_error: Option<&str>,
        error: Option<&str>,
    ) -> (bool, Vec<&'static str>, Vec<String>) {
        let mut blocked_reasons = Vec::new();
        if let Some(error) = refresh_error {
            blocked_reasons.push(format!("metadata refresh failed: {error}"));
        }

        match status {
            "upgrade_available" => {
                if !reviewed {
                    blocked_reasons.push("current commit is not reviewed".to_owned());
                }
            }
            "current" => {}
            "newer_than_srcinfo" => {
                blocked_reasons.push("installed version is newer than .SRCINFO".to_owned());
            }
            "missing_srcinfo" => blocked_reasons.push(".SRCINFO is missing".to_owned()),
            "not_installed" => blocked_reasons.push("package is not installed".to_owned()),
            "version_unknown" => blocked_reasons.push("available version is unknown".to_owned()),
            "error" => {
                blocked_reasons.push(error.unwrap_or("package plan has an error").to_owned())
            }
            _ => blocked_reasons.push(format!("unsupported status: {status}")),
        }

        let ready_for_build = status == "upgrade_available"
            && reviewed
            && refresh_error.is_none()
            && blocked_reasons.is_empty();
        let planned_actions = if ready_for_build {
            vec!["deps", "install-repo-deps", "build", "install-built"]
        } else {
            Vec::new()
        };

        (ready_for_build, planned_actions, blocked_reasons)
    }

    fn unresolved_dependency_summary(plan: &crate::model::DependencyPlan) -> String {
        let mut deps = Vec::new();
        deps.extend(plan.aur_deps.iter().map(|dep| format!("{dep} (AUR)")));
        deps.extend(
            plan.unknown_deps
                .iter()
                .map(|dep| format!("{dep} (unknown)")),
        );
        deps.extend(plan.provider_deps.iter().map(|provider| {
            format!(
                "{} (providers: {})",
                provider.dependency,
                provider.candidates.join(", ")
            )
        }));
        deps.join(", ")
    }

    fn yay_package_dirs(yay_build_dir: &Utf8Path) -> Result<BTreeMap<String, Utf8PathBuf>> {
        let mut dirs = BTreeMap::new();
        if !yay_build_dir.exists() {
            return Ok(dirs);
        }
        for entry in fs::read_dir(yay_build_dir)
            .with_context(|| format!("failed to read {yay_build_dir}"))?
        {
            let entry = entry?;
            let path = Utf8PathBuf::from_path_buf(entry.path())
                .map_err(|path| anyhow::anyhow!("non-UTF-8 path in yay cache: {path:?}"))?;
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            if path.join("PKGBUILD").exists() || path.join(".SRCINFO").exists() {
                dirs.insert(name.to_owned(), path);
            }
        }
        Ok(dirs)
    }

    fn git_head(user: &exec::BuildUser, path: &Utf8Path) -> Result<Option<String>> {
        if !path.join(".git").exists() {
            return Ok(None);
        }
        let output = match exec::run_as_user_capture(
            user,
            "git",
            vec!["rev-parse".to_owned(), "HEAD".to_owned()],
            path,
        ) {
            Ok(output) => output,
            Err(_) => {
                return Ok(None);
            }
        };
        let text = output.trim();
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(text.to_owned()))
    }

    fn git_current_branch(user: &exec::BuildUser, path: &Utf8Path) -> Result<String> {
        let output = exec::run_as_user_capture(
            user,
            "git",
            vec![
                "rev-parse".to_owned(),
                "--abbrev-ref".to_owned(),
                "HEAD".to_owned(),
            ],
            path,
        )
        .with_context(|| format!("failed to inspect git branch in {path}"))?;
        let branch = output.trim();
        if branch.is_empty() || branch == "HEAD" {
            bail!("{path} is not on a named branch");
        }
        Ok(branch.to_owned())
    }

    fn aur_repo_url(config: &Config, package: &str) -> String {
        format!("{}/{}.git", config.aur_url.trim_end_matches('/'), package)
    }

    fn validate_package_name(package: &str) -> Result<()> {
        if package.is_empty() || package.starts_with('-') {
            bail!("invalid package name: {package:?}");
        }
        if !package.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'@' | b'.' | b'_' | b'+' | b'-')
        }) {
            bail!("invalid package name: {package:?}");
        }
        Ok(())
    }

    fn package_path(config: &Config, db: &Database, package: &str) -> Result<Utf8PathBuf> {
        if let Some(record) = db.get_package(package)? {
            let path = Utf8PathBuf::from_path_buf(record.build_path.into())
                .map_err(|path| anyhow::anyhow!("state path is not UTF-8: {path:?}"))?;
            fs_safety::validate_absolute_normalized(&path, "stored package path")?;
            let allowed = path.parent() == Some(config.build_root.as_path())
                || path.parent() == Some(config.yay_build_dir.as_path());
            if !allowed {
                bail!("stored package path is outside build_root and yay_build_dir: {path}");
            }
            return Ok(path);
        }
        Ok(config.package_dir(package))
    }

    fn refresh_srcinfo(
        _config: &Config,
        user: &exec::BuildUser,
        package_dir: &Utf8Path,
    ) -> Result<()> {
        refresh_srcinfo_from_pkgbuild(user, package_dir)
    }

    fn refresh_srcinfo_from_pkgbuild(user: &exec::BuildUser, package_dir: &Utf8Path) -> Result<()> {
        if !package_dir.join("PKGBUILD").exists() {
            bail!("{package_dir} does not contain PKGBUILD");
        }
        exec::run_as_user(exec::UserCommand {
            user,
            program: "makepkg",
            args: vec!["--printsrcinfo".to_owned()],
            cwd: package_dir,
            stdout_file: Some(&package_dir.join(".SRCINFO")),
            stderr_file: Some(&package_dir.join(".aur-step-srcinfo.log")),
        })?;
        Ok(())
    }

    fn package_artifacts(
        package_dir: &Utf8Path,
        user: &exec::BuildUser,
    ) -> Result<Vec<Utf8PathBuf>> {
        let directory = fs_safety::open_directory_nofollow(package_dir)
            .with_context(|| format!("unsafe package directory {package_dir}"))?;
        let mut artifacts = Vec::new();
        for entry in
            fs::read_dir(package_dir).with_context(|| format!("failed to read {package_dir}"))?
        {
            let entry = entry?;
            let path = Utf8PathBuf::from_path_buf(entry.path())
                .map_err(|path| anyhow::anyhow!("non-UTF-8 path in {package_dir}: {path:?}"))?;
            let Some(name) = path.file_name() else {
                continue;
            };
            if is_package_artifact_name(name) {
                if !entry.file_type()?.is_file() {
                    bail!("package artifact is not a regular no-follow file: {path}");
                }
                let file = fs_safety::open_file_at_nofollow(directory.as_raw_fd(), name)
                    .with_context(|| format!("failed to open package artifact {path} safely"))?;
                validate_artifact_file(&file, &path, user)?;
                artifacts.push(path);
            }
        }
        artifacts.sort();
        Ok(artifacts)
    }

    fn validate_artifact_file(file: &File, path: &Utf8Path, user: &exec::BuildUser) -> Result<()> {
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to stat package artifact {path}"))?;
        if !metadata.file_type().is_file() {
            bail!("package artifact is not a regular file: {path}");
        }
        if metadata.uid() != user.uid {
            bail!(
                "package artifact has uid {}, expected build user {} ({}) for {path}",
                metadata.uid(),
                user.name,
                user.uid
            );
        }
        if metadata.nlink() != 1 {
            bail!("package artifact must not be hard-linked: {path}");
        }
        Ok(())
    }

    fn stage_package_artifacts(
        config: &Config,
        package_dir: &Utf8Path,
        artifacts: &[Utf8PathBuf],
        user: &exec::BuildUser,
    ) -> Result<(tempfile::TempDir, Vec<Utf8PathBuf>)> {
        let state_parent = fs_safety::state_parent(&config.state_db)?;
        let staging = tempfile::Builder::new()
            .prefix(".aur-step-artifacts-")
            .tempdir_in(&state_parent)
            .with_context(|| format!("failed to create artifact staging in {state_parent}"))?;
        fs::set_permissions(staging.path(), fs::Permissions::from_mode(0o700))
            .context("failed to protect artifact staging directory")?;
        let package_fd = fs_safety::open_directory_nofollow(package_dir)
            .with_context(|| format!("unsafe package directory {package_dir}"))?;
        let mut staged = Vec::with_capacity(artifacts.len());
        for artifact in artifacts {
            let name = artifact
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("artifact has no file name: {artifact}"))?;
            let mut source = fs_safety::open_file_at_nofollow(package_fd.as_raw_fd(), name)
                .with_context(|| format!("failed to reopen package artifact {artifact} safely"))?;
            validate_artifact_file(&source, artifact, user)?;
            let destination = staging.path().join(name);
            let mut destination_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&destination)
                .with_context(|| format!("failed to create staged artifact {destination:?}"))?;
            io::copy(&mut source, &mut destination_file)
                .with_context(|| format!("failed to stage package artifact {artifact}"))?;
            destination_file
                .sync_all()
                .with_context(|| format!("failed to sync staged artifact {destination:?}"))?;
            let destination = Utf8PathBuf::from_path_buf(destination)
                .map_err(|path| anyhow::anyhow!("staging path is not UTF-8: {path:?}"))?;
            validate_package_archive(&destination)?;
            staged.push(destination);
        }
        Ok((staging, staged))
    }

    fn validate_package_archive(path: &Utf8Path) -> Result<()> {
        let status = Command::new("/usr/bin/pacman")
            .args(["-Qp", path.as_str()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to validate staged package archive {path}"))?;
        if !status.success() {
            bail!("pacman rejected staged package archive {path}");
        }
        Ok(())
    }

    fn is_package_artifact_name(name: &str) -> bool {
        !name.ends_with(".sig")
            && (name.ends_with(".pkg.tar")
                || name.ends_with(".pkg.tar.zst")
                || name.ends_with(".pkg.tar.xz")
                || name.ends_with(".pkg.tar.gz")
                || name.ends_with(".pkg.tar.bz2")
                || name.ends_with(".pkg.tar.lz4")
                || name.ends_with(".pkg.tar.lzo"))
    }

    #[derive(Serialize)]
    struct InitOutput<'a> {
        build_user: &'a str,
        build_root: &'a str,
        state_db: &'a str,
    }

    #[derive(Serialize)]
    struct StatusOutput<'a> {
        build_user: &'a str,
        package_count: usize,
        packages: Vec<crate::model::PackageRecord>,
    }

    #[derive(Serialize)]
    struct FetchOutput {
        package: String,
        repo_url: String,
        path: Utf8PathBuf,
        last_commit: Option<String>,
    }

    #[derive(Serialize)]
    struct InspectOutput<'a> {
        package: &'a str,
        path: Utf8PathBuf,
        pkgbuild_path: Utf8PathBuf,
        srcinfo_path: Utf8PathBuf,
        pkgbuild_exists: bool,
        srcinfo_exists: bool,
        last_commit: Option<String>,
        reviewed: bool,
        review_diff: Option<ReviewDiffOutput>,
        state_record: Option<crate::model::PackageRecord>,
    }

    #[derive(Serialize)]
    struct ReviewDiffOutput {
        reviewed_commit: String,
        current_commit: Option<String>,
        status: &'static str,
        changed_files: Vec<GitChangedFile>,
        stat: Option<String>,
        diff: Option<String>,
        error: Option<String>,
    }

    #[derive(Serialize)]
    struct GitChangedFile {
        status: String,
        path: String,
        previous_path: Option<String>,
    }

    #[derive(Serialize)]
    struct RecursiveDepsOutput {
        root: String,
        inspected_count: usize,
        needs_fetch_count: usize,
        error_count: usize,
        packages: Vec<RecursiveDepsPackageOutput>,
    }

    #[derive(Serialize)]
    struct RecursiveDepsPackageOutput {
        package: String,
        required_by: Option<String>,
        status: &'static str,
        build_path: Utf8PathBuf,
        srcinfo_path: Option<Utf8PathBuf>,
        dependency_plan: Option<crate::model::DependencyPlan>,
        error: Option<String>,
    }

    #[derive(Serialize)]
    struct ReviewOutput<'a> {
        package: &'a str,
        path: Utf8PathBuf,
        reviewed_commit: String,
    }

    #[derive(Serialize)]
    struct RepoDepsOutput {
        package: String,
        plan_only: bool,
        pacman_args: Vec<String>,
        can_install: bool,
        selected_providers: Vec<ProviderSelection>,
        installed: Vec<String>,
        already_present: Vec<String>,
        provider_deps: Vec<crate::model::ProviderDependency>,
        aur_deps: Vec<String>,
        unknown_deps: Vec<String>,
        aur_or_unknown_deps: Vec<String>,
        srcinfo_path: Utf8PathBuf,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    struct ProviderSelection {
        dependency: String,
        package: String,
    }

    #[derive(Serialize)]
    struct BuildOutput {
        package: String,
        path: Utf8PathBuf,
        version: Option<String>,
        artifacts: Vec<Utf8PathBuf>,
    }

    #[derive(Serialize)]
    struct InstallBuiltOutput {
        package: String,
        plan_only: bool,
        path: Utf8PathBuf,
        version: Option<String>,
        pacman_args: Vec<String>,
        artifacts: Vec<Utf8PathBuf>,
    }

    #[derive(Serialize)]
    struct CleanOutput<'a> {
        package: &'a str,
        path: Utf8PathBuf,
        removed: Vec<CleanedPath>,
    }

    #[derive(Serialize)]
    struct CleanedPath {
        path: Utf8PathBuf,
        kind: &'static str,
    }

    #[derive(Serialize)]
    struct RemoveOutput {
        plan_only: bool,
        pacman_args: Vec<String>,
        packages: Vec<RemovePackageOutput>,
        state_removed: Vec<String>,
    }

    #[derive(Serialize)]
    struct RemovePackageOutput {
        package: String,
        managed: bool,
    }

    #[derive(Clone, Copy)]
    struct InstallOptions<'a> {
        assume_reviewed: bool,
        auto_aur_deps: bool,
        provider_selections: &'a [ProviderSelection],
    }

    struct InstallRunState {
        installing: Vec<String>,
        completed: Vec<String>,
        outputs: Vec<InstallPackageOutput>,
    }

    #[derive(Serialize)]
    struct InstallOutput {
        completed: Vec<String>,
        assume_reviewed: bool,
        auto_aur_deps: bool,
        packages: Vec<InstallPackageOutput>,
    }

    #[derive(Serialize)]
    struct InstallPackageOutput {
        package: String,
        required_by: Option<String>,
        fetch: FetchOutput,
        dependency_plan: crate::model::DependencyPlan,
        repo_dependency_install: RepoDepsOutput,
        build: BuildOutput,
        install: InstallBuiltOutput,
    }

    #[derive(Serialize)]
    struct YayImportOutput {
        yay_build_dir: Utf8PathBuf,
        imported_count: usize,
        imported: Vec<YayImportPackage>,
        missing_yay_cache: Vec<String>,
        cache_only: Vec<String>,
        cache_only_imported: bool,
    }

    #[derive(Serialize)]
    struct YayImportPackage {
        package: String,
        installed_version: String,
        build_path: Utf8PathBuf,
        has_yay_cache: bool,
        last_commit: Option<String>,
    }

    #[derive(Serialize)]
    struct UpgradeOutput<'a> {
        plan_only: bool,
        refresh: bool,
        repo_upgrade_required: bool,
        foreign_packages_error: Option<String>,
        package_count: usize,
        current_count: usize,
        upgrade_count: usize,
        missing_srcinfo_count: usize,
        not_installed_count: usize,
        error_count: usize,
        ready_for_build_count: usize,
        build_blocked_count: usize,
        packages: Vec<UpgradePackageOutput>,
        note: &'a str,
    }

    #[derive(Serialize)]
    struct UpgradeRunOutput<'a> {
        refresh: bool,
        repo_upgrade_args: Vec<String>,
        foreign_packages_error: Option<String>,
        package_count: usize,
        completed_count: usize,
        no_action_count: usize,
        blocked_count: usize,
        failed_count: usize,
        packages: Vec<UpgradeRunPackageOutput>,
        note: &'a str,
    }

    #[derive(Serialize)]
    struct UpgradeRunPackageOutput {
        package: String,
        status: &'static str,
        reviewed: bool,
        ready_for_build: bool,
        planned_actions: Vec<&'static str>,
        build_blocked_reasons: Vec<String>,
        dependency_install: Option<RepoDepsOutput>,
        build: Option<BuildOutput>,
        install: Option<InstallBuiltOutput>,
        result: &'static str,
        error: Option<String>,
    }

    #[derive(Serialize)]
    struct UpgradePackageOutput {
        package: String,
        status: &'static str,
        installed_version: Option<String>,
        available_version: Option<String>,
        comparison: Option<i32>,
        build_path: Utf8PathBuf,
        srcinfo_path: Utf8PathBuf,
        last_commit: Option<String>,
        reviewed: bool,
        ready_for_build: bool,
        planned_actions: Vec<&'static str>,
        build_blocked_reasons: Vec<String>,
        old_commit: Option<String>,
        new_commit: Option<String>,
        metadata_refreshed: bool,
        refresh_error: Option<String>,
        error: Option<String>,
    }

    #[cfg(test)]
    mod tests {
        use crate::{
            config::Config,
            db::Database,
            model::{DependencyPlan, ProviderDependency},
        };

        use super::{
            apply_provider_selections, execute_upgrade_package, is_package_artifact_name,
            parse_provider_selections, upgrade_build_plan, upgrade_status_from_comparison,
            validate_package_name, UpgradePackageOutput,
        };
        use camino::Utf8PathBuf;

        #[test]
        fn validates_arch_package_name_subset() {
            assert!(validate_package_name("visual-studio-code-bin").is_ok());
            assert!(validate_package_name("lib32-libidn11").is_ok());
            assert!(validate_package_name("foo+bar_baz@qux.1").is_ok());
            assert!(validate_package_name("../bad").is_err());
            assert!(validate_package_name("-bad").is_err());
            assert!(validate_package_name("").is_err());
        }

        #[test]
        fn recognizes_package_artifacts_without_signatures() {
            assert!(is_package_artifact_name("foo-1-1-x86_64.pkg.tar.zst"));
            assert!(is_package_artifact_name("foo-1-1-any.pkg.tar"));
            assert!(!is_package_artifact_name("foo-1-1-any.pkg.tar.zst.sig"));
            assert!(!is_package_artifact_name("foo.zip"));
        }

        #[test]
        fn maps_vercmp_results_to_upgrade_statuses() {
            assert_eq!(upgrade_status_from_comparison(-1), "upgrade_available");
            assert_eq!(upgrade_status_from_comparison(0), "current");
            assert_eq!(upgrade_status_from_comparison(1), "newer_than_srcinfo");
        }

        #[test]
        fn gates_upgrade_build_plan_on_reviewed_commit() {
            let (ready, actions, reasons) =
                upgrade_build_plan("upgrade_available", false, None, None);
            assert!(!ready);
            assert!(actions.is_empty());
            assert_eq!(reasons, ["current commit is not reviewed"]);

            let (ready, actions, reasons) =
                upgrade_build_plan("upgrade_available", true, None, None);
            assert!(ready);
            assert_eq!(
                actions,
                ["deps", "install-repo-deps", "build", "install-built"]
            );
            assert!(reasons.is_empty());
        }

        #[test]
        fn reports_upgrade_build_blockers() {
            let (ready, actions, reasons) =
                upgrade_build_plan("missing_srcinfo", true, Some("pull failed"), None);
            assert!(!ready);
            assert!(actions.is_empty());
            assert_eq!(
                reasons,
                [
                    "metadata refresh failed: pull failed",
                    ".SRCINFO is missing"
                ]
            );
        }

        #[test]
        fn blocked_upgrade_execution_does_not_run_package_steps() {
            let temp = tempfile::tempdir().unwrap();
            let config = Config {
                build_user: "nobody".to_owned(),
                build_root: Utf8PathBuf::from_path_buf(temp.path().join("build")).unwrap(),
                state_db: Utf8PathBuf::from_path_buf(temp.path().join("state.sqlite3")).unwrap(),
                yay_build_dir: Utf8PathBuf::from_path_buf(temp.path().join("yay")).unwrap(),
                aur_url: "file:///fake".to_owned(),
            };
            let db = Database::open(&config).unwrap();
            let package = UpgradePackageOutput {
                package: "fake-blocked".to_owned(),
                status: "upgrade_available",
                installed_version: Some("1.0-1".to_owned()),
                available_version: Some("1.1-1".to_owned()),
                comparison: Some(-1),
                build_path: config.build_root.join("fake-blocked"),
                srcinfo_path: config.build_root.join("fake-blocked/.SRCINFO"),
                last_commit: None,
                reviewed: false,
                ready_for_build: false,
                planned_actions: Vec::new(),
                build_blocked_reasons: vec!["current commit is not reviewed".to_owned()],
                old_commit: None,
                new_commit: None,
                metadata_refreshed: false,
                refresh_error: None,
                error: None,
            };

            let output = execute_upgrade_package(&config, &db, package, &[]);

            assert_eq!(output.result, "blocked");
            assert_eq!(output.package, "fake-blocked");
            assert!(output.dependency_install.is_none());
            assert!(output.build.is_none());
            assert!(output.install.is_none());
            assert_eq!(
                output.build_blocked_reasons,
                ["current commit is not reviewed"]
            );
        }

        #[test]
        fn current_upgrade_execution_is_no_action_not_blocked() {
            let temp = tempfile::tempdir().unwrap();
            let config = Config {
                build_user: "nobody".to_owned(),
                build_root: Utf8PathBuf::from_path_buf(temp.path().join("build")).unwrap(),
                state_db: Utf8PathBuf::from_path_buf(temp.path().join("state.sqlite3")).unwrap(),
                yay_build_dir: Utf8PathBuf::from_path_buf(temp.path().join("yay")).unwrap(),
                aur_url: "file:///fake".to_owned(),
            };
            let db = Database::open(&config).unwrap();
            let package = UpgradePackageOutput {
                package: "fake-current".to_owned(),
                status: "current",
                installed_version: Some("1.0-1".to_owned()),
                available_version: Some("1.0-1".to_owned()),
                comparison: Some(0),
                build_path: config.build_root.join("fake-current"),
                srcinfo_path: config.build_root.join("fake-current/.SRCINFO"),
                last_commit: None,
                reviewed: false,
                ready_for_build: false,
                planned_actions: Vec::new(),
                build_blocked_reasons: Vec::new(),
                old_commit: None,
                new_commit: None,
                metadata_refreshed: false,
                refresh_error: None,
                error: None,
            };

            let output = execute_upgrade_package(&config, &db, package, &[]);

            assert_eq!(output.result, "no_action");
            assert!(output.dependency_install.is_none());
            assert!(output.build.is_none());
            assert!(output.install.is_none());
            assert!(output.build_blocked_reasons.is_empty());
        }

        #[test]
        fn applies_explicit_provider_selection() {
            let mut plan = provider_test_plan();
            let selections = parse_provider_selections(&["ttf-font=noto-fonts".to_owned()])
                .expect("provider selection should parse");

            let selected =
                apply_provider_selections(&mut plan, &selections).expect("provider should apply");

            assert_eq!(selected, selections);
            assert!(plan.provider_deps.is_empty());
            assert_eq!(plan.repo_deps_missing, ["noto-fonts"]);
        }

        #[test]
        fn rejects_provider_selection_that_is_not_a_candidate() {
            let mut plan = provider_test_plan();
            let selections = parse_provider_selections(&["ttf-font=bad-fonts".to_owned()])
                .expect("provider selection should parse");

            let error = apply_provider_selections(&mut plan, &selections)
                .expect_err("invalid provider candidate should fail");

            assert!(error.to_string().contains("is not a candidate provider"));
            assert_eq!(plan.provider_deps.len(), 1);
            assert!(plan.repo_deps_missing.is_empty());
        }

        #[test]
        fn filters_provider_selections_to_current_dependency_plan() {
            let plan = provider_test_plan();
            let selections = parse_provider_selections(&[
                "ttf-font=noto-fonts".to_owned(),
                "other-virtual=other-package".to_owned(),
            ])
            .expect("provider selections should parse");

            let filtered = super::provider_selections_for_plan(&plan, &selections);
            let args = super::provider_selection_args_for_plan(&plan, &selections);

            assert_eq!(filtered, [selections[0].clone()]);
            assert_eq!(args, ["ttf-font=noto-fonts"]);
        }

        fn provider_test_plan() -> DependencyPlan {
            DependencyPlan {
                package: "fake".to_owned(),
                version: None,
                repo_deps_installed: Vec::new(),
                repo_deps_missing: Vec::new(),
                provider_deps: vec![ProviderDependency {
                    dependency: "ttf-font".to_owned(),
                    candidates: vec!["noto-fonts".to_owned()],
                }],
                aur_deps: Vec::new(),
                unknown_deps: Vec::new(),
                aur_or_unknown_deps: Vec::new(),
                optdepends: Vec::new(),
            }
        }
    }
}
