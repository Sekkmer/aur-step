use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn aur_step() -> &'static str {
    env!("CARGO_BIN_EXE_aur-step")
}

#[test]
fn deps_reads_fake_srcinfo_without_root_or_network() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-aur-step");
    fs::create_dir_all(&package_dir)?;
    fs::write(package_dir.join(".SRCINFO"), fake_srcinfo())?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "deps",
            "fake-aur-step",
        ])
        .output()
        .context("failed to run aur-step deps")?;
    assert!(
        output.status.success(),
        "aur-step deps failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["package"], "fake-aur-step");
    assert_eq!(value["version"], "1.2.3-4");
    assert_eq!(value["optdepends"].as_array().unwrap().len(), 1);
    Ok(())
}

#[test]
fn help_shows_provider_flags_for_high_level_commands() -> Result<()> {
    let install = Command::new(aur_step())
        .args(["install", "--help"])
        .output()
        .context("failed to run aur-step install --help")?;
    assert!(
        install.status.success(),
        "install --help failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let install_help = String::from_utf8(install.stdout)?;
    assert!(install_help.contains("--provider <PROVIDERS>"));
    assert!(install_help.contains("--auto-aur-deps"));
    assert!(install_help.contains("Fetch, review-gate, dependency-install"));
    assert!(install_help.contains("Examples:"));

    let upgrade = Command::new(aur_step())
        .args(["upgrade", "--help"])
        .output()
        .context("failed to run aur-step upgrade --help")?;
    assert!(
        upgrade.status.success(),
        "upgrade --help failed: {}",
        String::from_utf8_lossy(&upgrade.stderr)
    );
    let upgrade_help = String::from_utf8(upgrade.stdout)?;
    assert!(upgrade_help.contains("--provider <PROVIDERS>"));
    assert!(upgrade_help.contains("reviewed AUR package upgrades"));
    assert!(upgrade_help.contains("Examples:"));
    Ok(())
}

#[test]
fn config_path_rejects_symlinks() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;
    let link = temp.path().join("linked-config.toml");
    std::os::unix::fs::symlink(&config, &link)?;

    let output = Command::new(aur_step())
        .args(["--config", path_str(&link)?, "status"])
        .output()
        .context("failed to run config symlink rejection test")?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("without following symlinks"));
    Ok(())
}

#[test]
fn deps_splits_aur_and_unknown_dependencies() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let aur_root = temp.path().join("aur");
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-deps");
    fs::create_dir_all(aur_root.join("fake-aur-dep.git"))?;
    fs::create_dir_all(&package_dir)?;
    fs::write(
        package_dir.join(".SRCINFO"),
        r#"
pkgbase = fake-deps
	pkgver = 1.0.0
	pkgrel = 1
	arch = any
	pkgname = fake-deps
	depends = fake-aur-dep
	makedepends = fake-unknown-dep
"#,
    )?;
    let config = write_config(temp.path(), &build_root, &current_user_name()?, aur_root)?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "deps",
            "fake-deps",
        ])
        .output()
        .context("failed to run aur-step deps")?;
    assert!(
        output.status.success(),
        "aur-step deps failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["aur_deps"], serde_json::json!(["fake-aur-dep"]));
    assert_eq!(
        value["unknown_deps"],
        serde_json::json!(["fake-unknown-dep"])
    );
    assert_eq!(
        value["aur_or_unknown_deps"],
        serde_json::json!(["fake-unknown-dep"])
    );
    Ok(())
}

#[test]
fn recursive_deps_reports_inspected_and_needs_fetch_aur_packages() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let aur_root = temp.path().join("aur");
    let build_root = temp.path().join("aurbuild");
    let root_dir = build_root.join("fake-recursive-root");
    let dep_dir = build_root.join("fake-recursive-dep");
    fs::create_dir_all(aur_root.join("fake-recursive-dep.git"))?;
    fs::create_dir_all(aur_root.join("fake-recursive-missing.git"))?;
    fs::create_dir_all(&root_dir)?;
    fs::create_dir_all(&dep_dir)?;
    fs::write(
        root_dir.join(".SRCINFO"),
        r#"
pkgbase = fake-recursive-root
	pkgver = 1.0.0
	pkgrel = 1
	arch = any
	pkgname = fake-recursive-root
	depends = fake-recursive-dep
"#,
    )?;
    fs::write(
        dep_dir.join(".SRCINFO"),
        r#"
pkgbase = fake-recursive-dep
	pkgver = 1.0.0
	pkgrel = 1
	arch = any
	pkgname = fake-recursive-dep
	depends = fake-recursive-missing
"#,
    )?;
    let config = write_config(temp.path(), &build_root, &current_user_name()?, aur_root)?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "deps",
            "--recursive",
            "fake-recursive-root",
        ])
        .output()
        .context("failed to run aur-step deps --recursive")?;
    assert!(
        output.status.success(),
        "aur-step deps --recursive failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["root"], "fake-recursive-root");
    assert_eq!(value["inspected_count"], 2);
    assert_eq!(value["needs_fetch_count"], 1);
    let packages = value["packages"].as_array().unwrap();
    let root = packages
        .iter()
        .find(|package| package["package"] == "fake-recursive-root")
        .unwrap();
    let dep = packages
        .iter()
        .find(|package| package["package"] == "fake-recursive-dep")
        .unwrap();
    let missing = packages
        .iter()
        .find(|package| package["package"] == "fake-recursive-missing")
        .unwrap();
    assert_eq!(root["status"], "inspected");
    assert_eq!(
        root["dependency_plan"]["aur_deps"],
        serde_json::json!(["fake-recursive-dep"])
    );
    assert_eq!(dep["status"], "inspected");
    assert_eq!(dep["required_by"], "fake-recursive-root");
    assert_eq!(
        dep["dependency_plan"]["aur_deps"],
        serde_json::json!(["fake-recursive-missing"])
    );
    assert_eq!(missing["status"], "needs_fetch");
    assert_eq!(missing["required_by"], "fake-recursive-dep");
    assert!(missing["dependency_plan"].is_null());
    Ok(())
}

#[test]
fn plan_modes_do_not_require_root_or_call_pacman_install() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-aur-step");
    fs::create_dir_all(&package_dir)?;
    fs::write(package_dir.join(".SRCINFO"), fake_srcinfo())?;
    fs::write(
        package_dir.join("fake-aur-step-1.2.3-4-any.pkg.tar.zst"),
        b"not a real package; plan mode must not inspect contents",
    )?;
    chown_to_test_user_if_root(&package_dir.join("fake-aur-step-1.2.3-4-any.pkg.tar.zst"))?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;

    let repo_plan = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "install-repo-deps",
            "--plan",
            "fake-aur-step",
        ])
        .output()
        .context("failed to run aur-step install-repo-deps --plan")?;
    assert!(
        repo_plan.status.success(),
        "install-repo-deps --plan failed: {}",
        String::from_utf8_lossy(&repo_plan.stderr)
    );
    let repo_value: Value = serde_json::from_slice(&repo_plan.stdout)?;
    assert_eq!(repo_value["plan_only"], true);
    assert_eq!(repo_value["installed"].as_array().unwrap().len(), 0);

    let install_plan = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "install-built",
            "--plan",
            "fake-aur-step",
        ])
        .output()
        .context("failed to run aur-step install-built --plan")?;
    assert!(
        install_plan.status.success(),
        "install-built --plan failed: {}",
        String::from_utf8_lossy(&install_plan.stderr)
    );
    let install_value: Value = serde_json::from_slice(&install_plan.stdout)?;
    assert_eq!(install_value["plan_only"], true);
    assert_eq!(install_value["artifacts"].as_array().unwrap().len(), 1);
    Ok(())
}

#[test]
fn install_plan_rejects_symlinked_artifacts() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-symlink");
    fs::create_dir_all(&package_dir)?;
    fs::write(
        package_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-symlink", "1.0.0", "1"),
    )?;
    let target = temp.path().join("outside.pkg.tar.zst");
    fs::write(&target, b"not an artifact")?;
    std::os::unix::fs::symlink(
        &target,
        package_dir.join("fake-symlink-1.0.0-1-any.pkg.tar.zst"),
    )?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "install-built",
            "--plan",
            "fake-symlink",
        ])
        .output()
        .context("failed to run symlink artifact rejection test")?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("package artifact is not a regular no-follow file"));
    Ok(())
}

#[test]
fn clean_removes_only_generated_build_outputs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-clean");
    fs::create_dir_all(package_dir.join(".git"))?;
    fs::create_dir_all(package_dir.join("src"))?;
    fs::create_dir_all(package_dir.join("pkg"))?;
    fs::write(
        package_dir.join("PKGBUILD"),
        fake_pkgbuild_with_name("fake-clean", "1.0.0", "1"),
    )?;
    fs::write(
        package_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-clean", "1.0.0", "1"),
    )?;
    fs::write(package_dir.join("src/generated.txt"), b"generated")?;
    fs::write(package_dir.join("pkg/generated.txt"), b"generated")?;
    fs::write(
        package_dir.join("fake-clean-1.0.0-1-any.pkg.tar.zst"),
        b"artifact",
    )?;
    fs::write(
        package_dir.join("fake-clean-1.0.0-1-any.pkg.tar.zst.sig"),
        b"signature",
    )?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;
    seed_state(
        &temp.path().join("state.sqlite3"),
        "fake-clean",
        &package_dir,
        "1.0.0-1",
    )?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "clean",
            "fake-clean",
        ])
        .output()
        .context("failed to run aur-step clean")?;
    assert!(
        output.status.success(),
        "clean failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["package"], "fake-clean");
    assert_eq!(value["removed"].as_array().unwrap().len(), 3);
    assert!(!package_dir.join("src").exists());
    assert!(!package_dir.join("pkg").exists());
    assert!(!package_dir
        .join("fake-clean-1.0.0-1-any.pkg.tar.zst")
        .exists());
    assert!(package_dir.join(".git").exists());
    assert!(package_dir.join("PKGBUILD").exists());
    assert!(package_dir.join(".SRCINFO").exists());
    assert!(package_dir
        .join("fake-clean-1.0.0-1-any.pkg.tar.zst.sig")
        .exists());
    Ok(())
}

#[test]
fn remove_plan_does_not_require_root_or_delete_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-remove");
    fs::create_dir_all(&package_dir)?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;
    let db_path = temp.path().join("state.sqlite3");
    seed_state(&db_path, "fake-remove", &package_dir, "1.0.0-1")?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "remove",
            "--plan",
            "fake-remove",
            "fake-unmanaged",
        ])
        .output()
        .context("failed to run aur-step remove --plan")?;
    assert!(
        output.status.success(),
        "remove --plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["plan_only"], true);
    assert_eq!(
        value["pacman_args"],
        serde_json::json!(["-Rns", "--noconfirm", "fake-remove", "fake-unmanaged"])
    );
    assert!(value["state_removed"].as_array().unwrap().is_empty());
    let packages = value["packages"].as_array().unwrap();
    let managed = packages
        .iter()
        .find(|package| package["package"] == "fake-remove")
        .unwrap();
    let unmanaged = packages
        .iter()
        .find(|package| package["package"] == "fake-unmanaged")
        .unwrap();
    assert_eq!(managed["managed"], true);
    assert_eq!(unmanaged["managed"], false);

    let conn = Connection::open(db_path)?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packages WHERE aur_name = 'fake-remove'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn upgrade_plan_reads_state_and_existing_srcinfo() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let current_dir = build_root.join("fake-current");
    let missing_dir = build_root.join("fake-missing");
    fs::create_dir_all(&current_dir)?;
    fs::create_dir_all(&missing_dir)?;
    fs::write(
        current_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-current", "1.2.3", "4"),
    )?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;
    seed_state(
        &temp.path().join("state.sqlite3"),
        "fake-current",
        &current_dir,
        "1.2.3-4",
    )?;
    seed_state(
        &temp.path().join("state.sqlite3"),
        "fake-missing",
        &missing_dir,
        "1.0-1",
    )?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "upgrade",
            "--plan",
        ])
        .output()
        .context("failed to run aur-step upgrade --plan")?;
    assert!(
        output.status.success(),
        "upgrade --plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["plan_only"], true);
    assert_eq!(value["package_count"], 2);
    let packages = value["packages"].as_array().unwrap();
    let current = packages
        .iter()
        .find(|package| package["package"] == "fake-current")
        .unwrap();
    let missing = packages
        .iter()
        .find(|package| package["package"] == "fake-missing")
        .unwrap();
    assert_eq!(current["status"], "current");
    assert_eq!(current["comparison"], 0);
    assert_eq!(missing["status"], "missing_srcinfo");
    Ok(())
}

#[test]
fn upgrade_plan_actions_require_reviewed_checkout() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let reviewed_dir = build_root.join("fake-reviewed-upgrade");
    let unreviewed_dir = build_root.join("fake-unreviewed-upgrade");
    fs::create_dir_all(&reviewed_dir)?;
    fs::create_dir_all(&unreviewed_dir)?;
    fs::write(
        reviewed_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-reviewed-upgrade", "1.1.0", "1"),
    )?;
    fs::write(
        unreviewed_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-unreviewed-upgrade", "1.1.0", "1"),
    )?;
    init_git_fixture(&reviewed_dir, "reviewed fixture")?;
    init_git_fixture(&unreviewed_dir, "unreviewed fixture")?;
    let reviewed_commit = git_head(&reviewed_dir)?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;
    seed_state_with_review(
        &temp.path().join("state.sqlite3"),
        "fake-reviewed-upgrade",
        &reviewed_dir,
        "1.0.0-1",
        Some(&reviewed_commit),
    )?;
    seed_state(
        &temp.path().join("state.sqlite3"),
        "fake-unreviewed-upgrade",
        &unreviewed_dir,
        "1.0.0-1",
    )?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "upgrade",
            "--plan",
        ])
        .output()
        .context("failed to run aur-step upgrade --plan")?;
    assert!(
        output.status.success(),
        "upgrade --plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["upgrade_count"], 2);
    assert_eq!(value["ready_for_build_count"], 1);
    assert_eq!(value["build_blocked_count"], 1);
    let packages = value["packages"].as_array().unwrap();
    let reviewed = packages
        .iter()
        .find(|package| package["package"] == "fake-reviewed-upgrade")
        .unwrap();
    let unreviewed = packages
        .iter()
        .find(|package| package["package"] == "fake-unreviewed-upgrade")
        .unwrap();

    assert_eq!(reviewed["status"], "upgrade_available");
    assert_eq!(reviewed["reviewed"], true);
    assert_eq!(reviewed["ready_for_build"], true);
    assert_eq!(
        reviewed["planned_actions"],
        serde_json::json!(["deps", "install-repo-deps", "build", "install-built"])
    );
    assert!(reviewed["build_blocked_reasons"]
        .as_array()
        .unwrap()
        .is_empty());

    assert_eq!(unreviewed["status"], "upgrade_available");
    assert_eq!(unreviewed["reviewed"], false);
    assert_eq!(unreviewed["ready_for_build"], false);
    assert!(unreviewed["planned_actions"].as_array().unwrap().is_empty());
    assert_eq!(
        unreviewed["build_blocked_reasons"],
        serde_json::json!(["current commit is not reviewed"])
    );
    Ok(())
}

#[test]
fn review_records_current_checkout_commit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-review");
    fs::create_dir_all(&package_dir)?;
    fs::write(
        package_dir.join("PKGBUILD"),
        fake_pkgbuild_with_name("fake-review", "1.0.0", "1"),
    )?;
    fs::write(
        package_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-review", "1.0.0", "1"),
    )?;
    run("git", &["init"], &package_dir)?;
    run(
        "git",
        &["config", "user.email", "aur-step@example.invalid"],
        &package_dir,
    )?;
    run(
        "git",
        &["config", "user.name", "aur-step test"],
        &package_dir,
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &package_dir)?;
    run("git", &["commit", "-m", "review fixture"], &package_dir)?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;
    seed_state(
        &temp.path().join("state.sqlite3"),
        "fake-review",
        &package_dir,
        "1.0.0-1",
    )?;

    let review = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "review",
            "fake-review",
        ])
        .output()
        .context("failed to run aur-step review")?;
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    let review_value: Value = serde_json::from_slice(&review.stdout)?;
    assert!(review_value["reviewed_commit"].as_str().unwrap().len() >= 40);

    let inspect = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "inspect",
            "fake-review",
        ])
        .output()
        .context("failed to run aur-step inspect")?;
    assert!(
        inspect.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let inspect_value: Value = serde_json::from_slice(&inspect.stdout)?;
    assert_eq!(inspect_value["reviewed"], true);
    assert_eq!(
        inspect_value["state_record"]["reviewed_commit"],
        inspect_value["last_commit"]
    );
    assert_eq!(inspect_value["review_diff"]["status"], "current");
    assert_eq!(
        inspect_value["review_diff"]["reviewed_commit"],
        inspect_value["last_commit"]
    );
    Ok(())
}

#[test]
fn inspect_reports_diff_since_reviewed_commit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let build_root = temp.path().join("aurbuild");
    let package_dir = build_root.join("fake-review-diff");
    fs::create_dir_all(&package_dir)?;
    fs::write(
        package_dir.join("PKGBUILD"),
        fake_pkgbuild_with_name("fake-review-diff", "1.0.0", "1"),
    )?;
    fs::write(
        package_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-review-diff", "1.0.0", "1"),
    )?;
    run("git", &["init"], &package_dir)?;
    run(
        "git",
        &["config", "user.email", "aur-step@example.invalid"],
        &package_dir,
    )?;
    run(
        "git",
        &["config", "user.name", "aur-step test"],
        &package_dir,
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &package_dir)?;
    run("git", &["commit", "-m", "reviewed fixture"], &package_dir)?;
    let config = write_config(
        temp.path(),
        &build_root,
        &current_user_name()?,
        temp.path().join("aur"),
    )?;
    seed_state(
        &temp.path().join("state.sqlite3"),
        "fake-review-diff",
        &package_dir,
        "1.0.0-1",
    )?;

    let review = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "review",
            "fake-review-diff",
        ])
        .output()
        .context("failed to run aur-step review")?;
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );
    let reviewed_value: Value = serde_json::from_slice(&review.stdout)?;
    let reviewed_commit = reviewed_value["reviewed_commit"].as_str().unwrap();

    fs::write(
        package_dir.join("PKGBUILD"),
        fake_pkgbuild_with_name("fake-review-diff", "1.1.0", "1"),
    )?;
    fs::write(
        package_dir.join(".SRCINFO"),
        fake_named_srcinfo("fake-review-diff", "1.1.0", "1"),
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &package_dir)?;
    run("git", &["commit", "-m", "updated fixture"], &package_dir)?;

    let inspect = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "inspect",
            "fake-review-diff",
        ])
        .output()
        .context("failed to run aur-step inspect")?;
    assert!(
        inspect.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&inspect.stderr)
    );
    let inspect_value: Value = serde_json::from_slice(&inspect.stdout)?;
    assert_eq!(inspect_value["reviewed"], false);
    assert_eq!(inspect_value["review_diff"]["status"], "changed");
    assert_eq!(
        inspect_value["review_diff"]["reviewed_commit"],
        reviewed_commit
    );
    assert_ne!(
        inspect_value["review_diff"]["current_commit"],
        inspect_value["review_diff"]["reviewed_commit"]
    );
    let changed_paths = inspect_value["review_diff"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(changed_paths.contains(&"PKGBUILD"));
    assert!(changed_paths.contains(&".SRCINFO"));
    assert!(inspect_value["review_diff"]["diff"]
        .as_str()
        .unwrap()
        .contains("pkgver=1.1.0"));
    Ok(())
}

#[test]
fn root_fetch_clones_local_fake_aur_repo_as_build_user() -> Result<()> {
    if !root_integration_requested() {
        eprintln!("skipping root integration test; set AUR_STEP_RUN_ROOT_INTEGRATION=1 to enable");
        return Ok(());
    }
    if !is_root() {
        eprintln!("skipping root-only clone3 fetch integration test");
        return Ok(());
    }
    let Some(build_user) = usable_build_user() else {
        eprintln!("skipping clone3 fetch integration test; no usable non-root build user found");
        return Ok(());
    };

    let temp = tempfile::tempdir()?;
    chmod(temp.path(), 0o755)?;
    let aur_root = temp.path().join("aur");
    let source_root = temp.path().join("source");
    let build_root = temp.path().join("aurbuild");
    fs::create_dir_all(&aur_root)?;
    fs::create_dir_all(&source_root)?;
    fs::create_dir_all(&build_root)?;
    chmod(&aur_root, 0o755)?;
    chmod(&source_root, 0o755)?;
    chmod(&build_root, 0o755)?;

    let source_repo = source_root.join("fake-aur-step");
    let execution_sentinel = temp.path().join("fetch-must-not-evaluate-pkgbuild");
    fs::create_dir_all(&source_repo)?;
    fs::write(
        source_repo.join("PKGBUILD"),
        format!(
            "touch {}\n{}",
            execution_sentinel.display(),
            fake_pkgbuild()
        ),
    )?;
    fs::write(
        source_repo.join(".SRCINFO"),
        fake_build_srcinfo("fake-aur-step", "1.2.3", "4"),
    )?;
    run("git", &["init"], &source_repo)?;
    run(
        "git",
        &["config", "user.email", "aur-step@example.invalid"],
        &source_repo,
    )?;
    run(
        "git",
        &["config", "user.name", "aur-step test"],
        &source_repo,
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &source_repo)?;
    run("git", &["commit", "-m", "fake package"], &source_repo)?;
    run(
        "git",
        &[
            "clone",
            "--bare",
            path_str(&source_repo)?,
            path_str(&aur_root.join("fake-aur-step.git"))?,
        ],
        temp.path(),
    )?;
    run(
        "git",
        &[
            "remote",
            "add",
            "origin",
            path_str(&aur_root.join("fake-aur-step.git"))?,
        ],
        &source_repo,
    )?;
    chmod(&aur_root.join("fake-aur-step.git"), 0o755)?;

    let config = write_config(temp.path(), &build_root, &build_user, aur_root)?;
    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "fetch",
            "fake-aur-step",
        ])
        .output()
        .context("failed to run aur-step fetch")?;
    assert!(
        output.status.success(),
        "aur-step fetch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let checkout = build_root.join("fake-aur-step");
    assert!(checkout.join("PKGBUILD").exists());
    assert!(checkout.join(".SRCINFO").exists());
    assert!(
        !execution_sentinel.exists(),
        "fetch must not evaluate PKGBUILD before review"
    );
    let owner_uid = fs::metadata(checkout.join(".SRCINFO"))?.uid();
    assert_ne!(owner_uid, 0, ".SRCINFO must not be root-owned");
    Ok(())
}

#[test]
fn root_builds_local_fake_aur_package_as_build_user() -> Result<()> {
    if !root_integration_requested() {
        eprintln!("skipping root integration test; set AUR_STEP_RUN_ROOT_INTEGRATION=1 to enable");
        return Ok(());
    }
    if !is_root() {
        eprintln!("skipping root-only build integration test");
        return Ok(());
    }
    if !program_available("makepkg") || !program_available("fakeroot") {
        eprintln!("skipping build integration test; makepkg/fakeroot unavailable");
        return Ok(());
    }
    let Some(build_user) = usable_build_user() else {
        eprintln!("skipping build integration test; no usable non-root build user found");
        return Ok(());
    };

    let temp = tempfile::tempdir()?;
    chmod(temp.path(), 0o755)?;
    let aur_root = temp.path().join("aur");
    let source_root = temp.path().join("source");
    let build_root = temp.path().join("aurbuild");
    fs::create_dir_all(&aur_root)?;
    fs::create_dir_all(&source_root)?;
    fs::create_dir_all(&build_root)?;
    chmod(&aur_root, 0o755)?;
    chmod(&source_root, 0o755)?;
    chmod(&build_root, 0o755)?;

    let source_repo = source_root.join("fake-aur-step");
    fs::create_dir_all(&source_repo)?;
    fs::write(source_repo.join("PKGBUILD"), fake_pkgbuild())?;
    fs::write(
        source_repo.join(".SRCINFO"),
        fake_build_srcinfo("fake-aur-step", "1.2.3", "4"),
    )?;
    run("git", &["init"], &source_repo)?;
    run(
        "git",
        &["config", "user.email", "aur-step@example.invalid"],
        &source_repo,
    )?;
    run(
        "git",
        &["config", "user.name", "aur-step test"],
        &source_repo,
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &source_repo)?;
    run("git", &["commit", "-m", "fake package"], &source_repo)?;
    run(
        "git",
        &[
            "clone",
            "--bare",
            path_str(&source_repo)?,
            path_str(&aur_root.join("fake-aur-step.git"))?,
        ],
        temp.path(),
    )?;
    chmod(&aur_root.join("fake-aur-step.git"), 0o755)?;

    let config = write_config(temp.path(), &build_root, &build_user, aur_root)?;
    let fetch = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "fetch",
            "fake-aur-step",
        ])
        .output()
        .context("failed to run aur-step fetch before build")?;
    assert!(
        fetch.status.success(),
        "fetch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&fetch.stdout),
        String::from_utf8_lossy(&fetch.stderr)
    );

    let review = Command::new(aur_step())
        .args(["--config", path_str(&config)?, "review", "fake-aur-step"])
        .output()
        .context("failed to review fake package before build")?;
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );

    let build = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "build",
            "fake-aur-step",
        ])
        .output()
        .context("failed to run aur-step build")?;
    assert!(
        build.status.success(),
        "build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let value: Value = serde_json::from_slice(&build.stdout)?;
    assert_eq!(value["package"], "fake-aur-step");
    let artifact = value["artifacts"][0]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing built artifact in build output"))?;
    let artifact_path = Path::new(artifact);
    assert!(artifact_path.exists());
    assert_ne!(
        fs::metadata(artifact_path)?.uid(),
        0,
        "built package artifact must not be root-owned"
    );
    Ok(())
}

#[test]
fn root_install_built_installs_artifact_and_updates_state() -> Result<()> {
    if !root_integration_requested() {
        eprintln!("skipping root integration test; set AUR_STEP_RUN_ROOT_INTEGRATION=1 to enable");
        return Ok(());
    }
    if !is_root() {
        eprintln!("skipping root-only install-built integration test");
        return Ok(());
    }
    if !program_available("pacman")
        || !program_available("makepkg")
        || !program_available("fakeroot")
    {
        eprintln!("skipping install-built integration test; pacman/makepkg/fakeroot unavailable");
        return Ok(());
    }
    let Some(build_user) = usable_build_user() else {
        eprintln!("skipping install-built integration test; no usable non-root build user found");
        return Ok(());
    };

    let package_name = "fake-aur-step-installtest";
    if package_is_installed(package_name) {
        bail!(
            "refusing root install test because {package_name} was already installed before the test"
        );
    }
    let _installed_package = InstalledPackageGuard(package_name);

    let temp = tempfile::tempdir()?;
    chmod(temp.path(), 0o755)?;
    let aur_root = temp.path().join("aur");
    let source_root = temp.path().join("source");
    let build_root = temp.path().join("aurbuild");
    fs::create_dir_all(&aur_root)?;
    fs::create_dir_all(&source_root)?;
    fs::create_dir_all(&build_root)?;
    chmod(&aur_root, 0o755)?;
    chmod(&source_root, 0o755)?;
    chmod(&build_root, 0o755)?;

    let source_repo = source_root.join(package_name);
    fs::create_dir_all(&source_repo)?;
    fs::write(
        source_repo.join("PKGBUILD"),
        fake_pkgbuild_with_name(package_name, "1.2.3", "4"),
    )?;
    fs::write(
        source_repo.join(".SRCINFO"),
        fake_build_srcinfo(package_name, "1.2.3", "4"),
    )?;
    run("git", &["init"], &source_repo)?;
    run(
        "git",
        &["config", "user.email", "aur-step@example.invalid"],
        &source_repo,
    )?;
    run(
        "git",
        &["config", "user.name", "aur-step test"],
        &source_repo,
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &source_repo)?;
    run(
        "git",
        &["commit", "-m", "fake install package"],
        &source_repo,
    )?;
    run(
        "git",
        &[
            "clone",
            "--bare",
            path_str(&source_repo)?,
            path_str(&aur_root.join(format!("{package_name}.git")))?,
        ],
        temp.path(),
    )?;
    chmod(&aur_root.join(format!("{package_name}.git")), 0o755)?;

    let config = write_config(temp.path(), &build_root, &build_user, aur_root)?;
    let fetch = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "fetch",
            package_name,
        ])
        .output()
        .context("failed to run aur-step fetch before install-built")?;
    assert!(
        fetch.status.success(),
        "fetch failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&fetch.stdout),
        String::from_utf8_lossy(&fetch.stderr)
    );

    let review = Command::new(aur_step())
        .args(["--config", path_str(&config)?, "review", package_name])
        .output()
        .context("failed to review fake package before install build")?;
    assert!(
        review.status.success(),
        "review failed: {}",
        String::from_utf8_lossy(&review.stderr)
    );

    let build = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "build",
            package_name,
        ])
        .output()
        .context("failed to run aur-step build before install-built")?;
    assert!(
        build.status.success(),
        "build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let install = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "install-built",
            package_name,
        ])
        .output()
        .context("failed to run aur-step install-built")?;
    let install_stdout = String::from_utf8_lossy(&install.stdout);
    let install_stderr = String::from_utf8_lossy(&install.stderr);
    assert!(
        install.status.success(),
        "install-built failed\nstdout:\n{}\nstderr:\n{}",
        install_stdout,
        install_stderr
    );
    let value: Value = serde_json::from_slice(&install.stdout)?;
    assert_eq!(value["package"], package_name);
    assert_eq!(value["version"], "1.2.3-4");

    let query = Command::new("pacman")
        .args(["-Q", package_name])
        .output()
        .context("failed to query installed fake package")?;
    assert!(
        query.status.success(),
        "pacman -Q did not find installed fake package: {}",
        String::from_utf8_lossy(&query.stderr)
    );
    let conn = Connection::open(temp.path().join("state.sqlite3"))?;
    let installed_version: Option<String> = conn.query_row(
        "SELECT last_installed_version FROM packages WHERE aur_name = ?1",
        params![package_name],
        |row| row.get(0),
    )?;
    assert_eq!(installed_version.as_deref(), Some("1.2.3-4"));

    Ok(())
}

#[test]
fn root_upgrade_plan_refreshes_fake_aur_metadata_as_build_user() -> Result<()> {
    if !root_integration_requested() {
        eprintln!("skipping root integration test; set AUR_STEP_RUN_ROOT_INTEGRATION=1 to enable");
        return Ok(());
    }
    if !is_root() {
        eprintln!("skipping root-only upgrade refresh integration test");
        return Ok(());
    }
    let Some(build_user) = usable_build_user() else {
        eprintln!("skipping upgrade refresh integration test; no usable non-root build user found");
        return Ok(());
    };

    let temp = tempfile::tempdir()?;
    chmod(temp.path(), 0o755)?;
    let aur_root = temp.path().join("aur");
    let source_root = temp.path().join("source");
    let build_root = temp.path().join("aurbuild");
    fs::create_dir_all(&aur_root)?;
    fs::create_dir_all(&source_root)?;
    fs::create_dir_all(&build_root)?;
    chmod(&aur_root, 0o755)?;
    chmod(&source_root, 0o755)?;
    chmod(&build_root, 0o755)?;

    let source_repo = source_root.join("fake-aur-step");
    fs::create_dir_all(&source_repo)?;
    fs::write(
        source_repo.join("PKGBUILD"),
        fake_pkgbuild_with_version("1.0.0", "1"),
    )?;
    fs::write(
        source_repo.join(".SRCINFO"),
        fake_named_srcinfo("fake-aur-step", "1.0.0", "1"),
    )?;
    run("git", &["init"], &source_repo)?;
    run(
        "git",
        &["config", "user.email", "aur-step@example.invalid"],
        &source_repo,
    )?;
    run(
        "git",
        &["config", "user.name", "aur-step test"],
        &source_repo,
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &source_repo)?;
    run("git", &["commit", "-m", "fake package 1.0.0"], &source_repo)?;
    run(
        "git",
        &[
            "clone",
            "--bare",
            path_str(&source_repo)?,
            path_str(&aur_root.join("fake-aur-step.git"))?,
        ],
        temp.path(),
    )?;
    run(
        "git",
        &[
            "remote",
            "add",
            "origin",
            path_str(&aur_root.join("fake-aur-step.git"))?,
        ],
        &source_repo,
    )?;
    chmod(&aur_root.join("fake-aur-step.git"), 0o755)?;

    let config = write_config(temp.path(), &build_root, &build_user, aur_root)?;
    let fetch = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "fetch",
            "fake-aur-step",
        ])
        .output()
        .context("failed to run initial aur-step fetch")?;
    assert!(
        fetch.status.success(),
        "initial fetch failed: {}",
        String::from_utf8_lossy(&fetch.stderr)
    );

    fs::write(
        source_repo.join("PKGBUILD"),
        fake_pkgbuild_with_version("1.1.0", "1"),
    )?;
    fs::write(
        source_repo.join(".SRCINFO"),
        fake_named_srcinfo("fake-aur-step", "1.1.0", "1"),
    )?;
    run("git", &["add", "PKGBUILD", ".SRCINFO"], &source_repo)?;
    run("git", &["commit", "-m", "fake package 1.1.0"], &source_repo)?;
    run("git", &["push", "origin", "master"], &source_repo)?;
    seed_state(
        &temp.path().join("state.sqlite3"),
        "fake-aur-step",
        &build_root.join("fake-aur-step"),
        "1.0.0-1",
    )?;

    let output = Command::new(aur_step())
        .args([
            "--config",
            path_str(&config)?,
            "--json",
            "upgrade",
            "--plan",
            "--refresh",
        ])
        .output()
        .context("failed to run aur-step upgrade --plan --refresh")?;
    assert!(
        output.status.success(),
        "upgrade --plan --refresh failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    let package = value["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["package"] == "fake-aur-step")
        .unwrap();
    assert_eq!(
        package["status"], "upgrade_available",
        "unexpected refreshed package plan: {package:#}"
    );
    assert_eq!(package["installed_version"], "1.0.0-1");
    assert_eq!(package["available_version"], "1.1.0-1");
    assert_eq!(package["metadata_refreshed"], true);
    assert_ne!(package["old_commit"], package["new_commit"]);
    let owner_uid = fs::metadata(build_root.join("fake-aur-step/.SRCINFO"))?.uid();
    assert_ne!(
        owner_uid, 0,
        ".SRCINFO must not be root-owned after refresh"
    );
    Ok(())
}

fn write_config(
    temp: &Path,
    build_root: &Path,
    build_user: &str,
    aur_root: PathBuf,
) -> Result<PathBuf> {
    let config = temp.join("aur-step.toml");
    fs::write(
        &config,
        format!(
            "state_db = \"{}\"\nbuild_root = \"{}\"\nyay_build_dir = \"{}\"\nbuild_user = \"{}\"\naur_url = \"file://{}\"\n",
            temp.join("state.sqlite3").display(),
            build_root.display(),
            temp.join("yay").display(),
            build_user,
            aur_root.display(),
        ),
    )?;
    Ok(config)
}

fn fake_pkgbuild() -> String {
    fake_pkgbuild_with_version("1.2.3", "4")
}

fn fake_pkgbuild_with_version(pkgver: &str, pkgrel: &str) -> String {
    fake_pkgbuild_with_name("fake-aur-step", pkgver, pkgrel)
}

fn fake_pkgbuild_with_name(name: &str, pkgver: &str, pkgrel: &str) -> String {
    format!(
        r#"pkgname={name}
pkgver={pkgver}
pkgrel={pkgrel}
pkgdesc='fake package for aur-step tests'
arch=('any')
license=('MIT')

package() {{
  mkdir -p "$pkgdir/usr/share/{name}"
  printf 'ok\n' > "$pkgdir/usr/share/{name}/readme.txt"
}}
"#
    )
}

fn fake_srcinfo() -> &'static str {
    r#"
pkgbase = fake-aur-step
	pkgdesc = fake package for aur-step tests
	pkgver = 1.2.3
	pkgrel = 4
	arch = any
	pkgname = fake-aur-step
	optdepends = optional-tool: optional test dependency
"#
}

fn fake_named_srcinfo(name: &str, pkgver: &str, pkgrel: &str) -> String {
    format!(
        r#"
pkgbase = {name}
	pkgver = {pkgver}
	pkgrel = {pkgrel}
	arch = any
	pkgname = {name}
"#
    )
}

fn fake_build_srcinfo(name: &str, pkgver: &str, pkgrel: &str) -> String {
    format!(
        "pkgbase = {name}\n\tpkgdesc = fake package for aur-step tests\n\tpkgver = {pkgver}\n\tpkgrel = {pkgrel}\n\tarch = any\n\tlicense = MIT\n\npkgname = {name}\n"
    )
}

fn seed_state(
    db_path: &Path,
    package: &str,
    build_path: &Path,
    installed_version: &str,
) -> Result<()> {
    seed_state_with_review(db_path, package, build_path, installed_version, None)
}

fn seed_state_with_review(
    db_path: &Path,
    package: &str,
    build_path: &Path,
    installed_version: &str,
    reviewed_commit: Option<&str>,
) -> Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS packages (
            aur_name TEXT PRIMARY KEY,
            repo_url TEXT NOT NULL,
            build_path TEXT NOT NULL,
            last_built_version TEXT,
            last_installed_version TEXT,
            last_commit TEXT,
            reviewed_commit TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )?;
    conn.execute(
        r#"
        INSERT INTO packages (
            aur_name, repo_url, build_path, last_installed_version,
            last_commit, reviewed_commit, created_at, updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'now', 'now')
        ON CONFLICT(aur_name) DO UPDATE SET
            build_path = excluded.build_path,
            last_installed_version = excluded.last_installed_version,
            last_commit = excluded.last_commit,
            reviewed_commit = excluded.reviewed_commit,
            updated_at = excluded.updated_at
        "#,
        params![
            package,
            format!("file:///fake/{package}.git"),
            path_str(build_path)?,
            installed_version,
            reviewed_commit,
            reviewed_commit
        ],
    )?;
    Ok(())
}

fn init_git_fixture(path: &Path, message: &str) -> Result<()> {
    run("git", &["init"], path)?;
    run(
        "git",
        &["config", "user.email", "aur-step@example.invalid"],
        path,
    )?;
    run("git", &["config", "user.name", "aur-step test"], path)?;
    run("git", &["add", ".SRCINFO"], path)?;
    run("git", &["commit", "-m", message], path)?;
    Ok(())
}

fn git_head(path: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()?;
    if !output.status.success() {
        bail!(
            "git rev-parse failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Result<()> {
    let output = Command::new(program).args(args).current_dir(cwd).output()?;
    if !output.status.success() {
        bail!(
            "{program} failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("path is not UTF-8: {path:?}"))
}

fn chmod(path: &Path, mode: u32) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn chown_to_test_user_if_root(path: &Path) -> Result<()> {
    if !is_root() {
        return Ok(());
    }
    let user = usable_build_user().ok_or_else(|| anyhow::anyhow!("no non-root test user"))?;
    let status = Command::new("chown")
        .args([user.as_str(), path_str(path)?])
        .status()?;
    if !status.success() {
        bail!("failed to chown {} to {user}", path.display());
    }
    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn root_integration_requested() -> bool {
    std::env::var("AUR_STEP_RUN_ROOT_INTEGRATION").as_deref() == Ok("1")
}

fn current_user_name() -> Result<String> {
    if is_root() {
        if let Some(user) = usable_build_user() {
            return Ok(user);
        }
    }
    let output = Command::new("id")
        .arg("-un")
        .output()
        .context("failed to look up current test user")?;
    if !output.status.success() {
        bail!("id -un failed");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn usable_build_user() -> Option<String> {
    std::env::var("AUR_STEP_ROOT_TEST_USER")
        .ok()
        .filter(|user| user != "root")
        .or_else(|| {
            std::env::var("SUDO_USER")
                .ok()
                .filter(|user| user != "root")
        })
        .or_else(|| {
            Command::new("logname")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|user| user.trim().to_owned())
                .filter(|user| !user.is_empty() && user != "root")
        })
}

fn program_available(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn pacman_remove_if_installed(package: &str) {
    if package_is_installed(package) {
        let _ = Command::new("pacman")
            .args(["-Rns", "--noconfirm", package])
            .status();
    }
}

fn package_is_installed(package: &str) -> bool {
    Command::new("pacman")
        .args(["-Q", package])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

struct InstalledPackageGuard(&'static str);

impl Drop for InstalledPackageGuard {
    fn drop(&mut self) {
        pacman_remove_if_installed(self.0);
    }
}

trait MetadataUid {
    fn uid(&self) -> u32;
}

impl MetadataUid for fs::Metadata {
    fn uid(&self) -> u32 {
        std::os::unix::fs::MetadataExt::uid(self)
    }
}
