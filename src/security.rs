use anyhow::{bail, Context, Result};
use camino::Utf8Path;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::process::Command;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecurityFinding {
    pub code: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactAudit {
    pub sha256: String,
    pub manifest_sha256: String,
    pub entry_count: usize,
    pub privileged_findings: Vec<SecurityFinding>,
}

pub fn source_findings(srcinfo: &str, repository_files: &[String]) -> Vec<SecurityFinding> {
    let mut findings = Vec::new();
    for line in srcinfo.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.contains("sums") && value.eq_ignore_ascii_case("SKIP") {
            findings.push(SecurityFinding {
                code: "source_checksum_skipped",
                detail: format!("{key} contains SKIP"),
            });
        }
        if key == "install" {
            findings.push(SecurityFinding {
                code: "package_install_script",
                detail: format!("package declares install script {value}"),
            });
        }
        if key == "source" || key.starts_with("source_") {
            let lowered = value.to_ascii_lowercase();
            if lowered.starts_with("git+")
                || lowered.starts_with("hg+")
                || lowered.starts_with("svn+")
                || lowered.contains("/latest/")
                || lowered.ends_with("/latest")
            {
                findings.push(SecurityFinding {
                    code: "mutable_source",
                    detail: format!("{key} is mutable: {value}"),
                });
            }
        }
    }
    for file in repository_files {
        if file == ".install" || file.ends_with(".install") {
            findings.push(SecurityFinding {
                code: "repository_install_script",
                detail: format!("repository contains {file}"),
            });
        }
    }
    findings.sort_by(|left, right| {
        (left.code, left.detail.as_str()).cmp(&(right.code, right.detail.as_str()))
    });
    findings.dedup();
    findings
}

pub fn source_values(srcinfo: &str) -> Vec<String> {
    let mut sources = srcinfo
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| {
            let key = key.trim();
            key == "source" || key.starts_with("source_")
        })
        .map(|(_, value)| value.trim().to_owned())
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    sources
}

pub fn audit_package(path: &Utf8Path) -> Result<ArtifactAudit> {
    let manifest = command_output("/usr/bin/bsdtar", &["-tf", path.as_str()])
        .with_context(|| format!("failed to list package archive {path}"))?;
    let verbose = command_output("/usr/bin/bsdtar", &["-tvf", path.as_str()])
        .with_context(|| format!("failed to inspect package archive modes for {path}"))?;
    let entries = manifest
        .lines()
        .map(normalize_archive_path)
        .collect::<Vec<_>>();
    let mut findings = Vec::new();

    for entry in &entries {
        if entry.is_empty() {
            continue;
        }
        if entry.starts_with('/') || entry.split('/').any(|component| component == "..") {
            bail!("package archive contains unsafe path {entry:?}: {path}");
        }
        if let Some(code) = privileged_path_code(entry) {
            findings.push(SecurityFinding {
                code,
                detail: format!("package archive contains {entry}"),
            });
        }
    }

    for line in verbose.lines() {
        let Some(mode) = line.split_whitespace().next() else {
            continue;
        };
        let entry = line.split_whitespace().last().unwrap_or("unknown");
        if mode.starts_with('b') || mode.starts_with('c') {
            findings.push(SecurityFinding {
                code: "device_node",
                detail: format!("package archive contains device node {entry}"),
            });
        }
        if mode
            .as_bytes()
            .get(3)
            .is_some_and(|byte| matches!(byte, b's' | b'S'))
            || mode
                .as_bytes()
                .get(6)
                .is_some_and(|byte| matches!(byte, b's' | b'S'))
        {
            findings.push(SecurityFinding {
                code: "setid_file",
                detail: format!("package archive contains setuid/setgid entry {entry}"),
            });
        }
    }

    findings.sort_by(|left, right| {
        (left.code, left.detail.as_str()).cmp(&(right.code, right.detail.as_str()))
    });
    findings.dedup();
    Ok(ArtifactAudit {
        sha256: sha256_file(path)?,
        manifest_sha256: sha256_bytes(manifest.as_bytes()),
        entry_count: entries.len(),
        privileged_findings: findings,
    })
}

pub fn sha256_file(path: &Utf8Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("failed to hash {path}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn command_output(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("archive tool output was not UTF-8")
}

fn normalize_archive_path(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}

fn privileged_path_code(path: &str) -> Option<&'static str> {
    if path == ".INSTALL" {
        return Some("package_install_script");
    }
    const PREFIXES: &[(&str, &str)] = &[
        ("etc/sudoers", "sudo_configuration"),
        ("etc/sudoers.d/", "sudo_configuration"),
        ("etc/pam.d/", "pam_configuration"),
        ("etc/ssh/", "ssh_configuration"),
        ("etc/cron", "scheduled_task"),
        ("var/spool/cron/", "scheduled_task"),
        ("etc/systemd/system/", "systemd_unit"),
        ("usr/lib/systemd/system/", "systemd_unit"),
        ("usr/lib/systemd/system-generators/", "systemd_generator"),
        ("etc/pacman.d/hooks/", "pacman_hook"),
        ("usr/share/libalpm/hooks/", "pacman_hook"),
        ("etc/polkit-1/rules.d/", "polkit_rule"),
        ("usr/share/polkit-1/rules.d/", "polkit_rule"),
        ("etc/tmpfiles.d/", "tmpfiles_rule"),
        ("usr/lib/tmpfiles.d/", "tmpfiles_rule"),
        ("etc/sysusers.d/", "sysusers_rule"),
        ("usr/lib/sysusers.d/", "sysusers_rule"),
    ];
    PREFIXES
        .iter()
        .find(|(prefix, _)| path == prefix.trim_end_matches('/') || path.starts_with(prefix))
        .map(|(_, code)| *code)
}

#[cfg(test)]
mod tests {
    use super::{audit_package, source_findings};
    use camino::Utf8PathBuf;
    use std::{fs, process::Command};

    #[test]
    fn detects_source_review_risks() {
        let findings = source_findings(
            "source = git+https://example.invalid/project.git\nsha256sums = SKIP\ninstall = bad.install\n",
            &["bad.install".to_owned()],
        );
        let codes = findings
            .iter()
            .map(|finding| finding.code)
            .collect::<Vec<_>>();
        assert!(codes.contains(&"mutable_source"));
        assert!(codes.contains(&"source_checksum_skipped"));
        assert!(codes.contains(&"package_install_script"));
        assert!(codes.contains(&"repository_install_script"));
    }

    #[test]
    fn audits_privileged_archive_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("usr/lib/systemd/system")).unwrap();
        fs::write(root.join(".INSTALL"), "post_install() { :; }").unwrap();
        fs::write(
            root.join("usr/lib/systemd/system/evil.service"),
            "[Service]\nExecStart=/usr/bin/false\n",
        )
        .unwrap();
        let archive = temp.path().join("test.pkg.tar");
        let status = Command::new("/usr/bin/bsdtar")
            .args(["-cf", archive.to_str().unwrap(), "."])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(status.success());
        let archive = Utf8PathBuf::from_path_buf(archive).unwrap();
        let audit = audit_package(&archive).unwrap();
        let codes = audit
            .privileged_findings
            .iter()
            .map(|finding| finding.code)
            .collect::<Vec<_>>();
        assert!(codes.contains(&"package_install_script"));
        assert!(codes.contains(&"systemd_unit"));
        assert_eq!(audit.sha256.len(), 64);
        assert_eq!(audit.manifest_sha256.len(), 64);
    }
}
