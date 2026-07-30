use crate::model::{DependencyPlan, ProviderDependency, SrcInfo};
use anyhow::Result;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn classify(srcinfo: &SrcInfo) -> Result<DependencyPlan> {
    let package = srcinfo
        .pkgbase
        .clone()
        .or_else(|| srcinfo.pkgname.first().cloned())
        .unwrap_or_else(|| "unknown".to_owned());
    let version = srcinfo.version();

    let mut deps = Vec::new();
    deps.extend(srcinfo.depends.iter().map(|dep| normalize_dep(dep)));
    deps.extend(srcinfo.makedepends.iter().map(|dep| normalize_dep(dep)));
    deps.extend(srcinfo.checkdepends.iter().map(|dep| normalize_dep(dep)));
    deps.sort();
    deps.dedup();

    let mut repo_deps_installed = Vec::new();
    let mut repo_deps_missing = Vec::new();
    let mut provider_deps = Vec::new();
    let mut aur_or_unknown_deps = Vec::new();

    for dep in deps {
        if is_installed(&dep)? {
            repo_deps_installed.push(dep);
        } else if is_repo_package(&dep)? {
            repo_deps_missing.push(dep);
        } else {
            let candidates = provider_candidates(&dep)?;
            if candidates.is_empty() {
                aur_or_unknown_deps.push(dep);
            } else {
                provider_deps.push(ProviderDependency {
                    dependency: dep,
                    candidates,
                });
            }
        }
    }

    Ok(DependencyPlan {
        package,
        version,
        repo_deps_installed,
        repo_deps_missing,
        provider_deps,
        aur_deps: Vec::new(),
        unknown_deps: Vec::new(),
        aur_or_unknown_deps,
        optdepends: srcinfo.optdepends.clone(),
    })
}

fn normalize_dep(dep: &str) -> String {
    let dep = dep.split(':').next().unwrap_or(dep).trim();
    for op in [">=", "<=", "=", ">", "<"] {
        if let Some((name, _)) = dep.split_once(op) {
            return name.trim().to_owned();
        }
    }
    dep.to_owned()
}

fn is_installed(dep: &str) -> Result<bool> {
    let output = Command::new("/usr/bin/pacman")
        .args(["-T", dep])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Ok(false);
    };
    Ok(output.status.success())
}

fn is_repo_package(dep: &str) -> Result<bool> {
    let status = Command::new("/usr/bin/pacman")
        .args(["-Si", dep])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn provider_candidates(dep: &str) -> Result<Vec<String>> {
    sync_db_provider_candidates(dep)
}

fn sync_db_provider_candidates(dep: &str) -> Result<Vec<String>> {
    let sync_dir = Path::new("/var/lib/pacman/sync");
    if !sync_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(sync_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("db") {
            continue;
        }
        let output = Command::new("/usr/bin/bsdtar")
            .args(["-xOf", path.to_string_lossy().as_ref(), "*/desc"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();
        let Ok(output) = output else {
            return Ok(Vec::new());
        };
        if !output.status.success() {
            continue;
        }
        collect_provider_candidates_from_desc(
            &String::from_utf8_lossy(&output.stdout),
            dep,
            &mut candidates,
        );
        if candidates.len() >= 20 {
            break;
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates.truncate(20);
    Ok(candidates)
}

fn collect_provider_candidates_from_desc(text: &str, dep: &str, candidates: &mut Vec<String>) {
    let mut name = None::<String>;
    let mut provides = Vec::<String>::new();
    let mut section = None::<&str>;

    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('%') && line.ends_with('%') {
            let next_section = line.trim_matches('%');
            if next_section == "FILENAME" {
                push_provider_candidate(dep, candidates, name.take(), &provides);
                provides.clear();
            }
            section = Some(next_section);
            continue;
        }

        match section {
            Some("NAME") => name = Some(line.to_owned()),
            Some("PROVIDES") => provides.push(normalize_dep(line)),
            _ => {}
        }
    }
    push_provider_candidate(dep, candidates, name, &provides);
}

fn push_provider_candidate(
    dep: &str,
    candidates: &mut Vec<String>,
    name: Option<String>,
    provides: &[String],
) {
    if provides.iter().any(|provided| provided == dep) {
        if let Some(name) = name {
            candidates.push(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{collect_provider_candidates_from_desc, normalize_dep};

    #[test]
    fn strips_version_constraints() {
        assert_eq!(normalize_dep("foo>=1.0"), "foo");
        assert_eq!(normalize_dep("bar=2"), "bar");
        assert_eq!(normalize_dep("baz: optional"), "baz");
    }

    #[test]
    fn parses_provider_candidates_from_sync_desc() {
        let text = r#"%FILENAME%
noto-fonts-1-any.pkg.tar.zst

%NAME%
noto-fonts

%PROVIDES%
ttf-font

%FILENAME%
other-1-any.pkg.tar.zst

%NAME%
other

%PROVIDES%
something-else
"#;
        let mut candidates = Vec::new();
        collect_provider_candidates_from_desc(text, "ttf-font", &mut candidates);
        assert_eq!(candidates, ["noto-fonts"]);
    }
}
