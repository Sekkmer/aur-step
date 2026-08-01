use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PackageRecord {
    pub aur_name: String,
    pub repo_url: String,
    pub build_path: String,
    pub last_built_version: Option<String>,
    pub last_installed_version: Option<String>,
    pub last_commit: Option<String>,
    pub reviewed_commit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SrcInfo {
    pub pkgbase: Option<String>,
    pub pkgname: Vec<String>,
    pub pkgver: Option<String>,
    pub pkgrel: Option<String>,
    pub depends: Vec<String>,
    pub makedepends: Vec<String>,
    pub checkdepends: Vec<String>,
    pub optdepends: Vec<String>,
}

impl SrcInfo {
    pub fn version(&self) -> Option<String> {
        match (&self.pkgver, &self.pkgrel) {
            (Some(ver), Some(rel)) => Some(format!("{ver}-{rel}")),
            (Some(ver), None) => Some(ver.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyPlan {
    pub package: String,
    pub version: Option<String>,
    pub repo_deps_installed: Vec<String>,
    pub repo_deps_missing: Vec<String>,
    pub provider_deps: Vec<ProviderDependency>,
    pub aur_deps: Vec<String>,
    pub unknown_deps: Vec<String>,
    pub aur_or_unknown_deps: Vec<String>,
    pub optdepends: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderDependency {
    pub dependency: String,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustRecord {
    pub observed_maintainer: Option<String>,
    pub reviewed_maintainer: Option<String>,
    pub observed_at: Option<String>,
    pub reviewed_at: Option<String>,
    pub observed_sources: Vec<String>,
    pub reviewed_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArtifactRecord {
    pub path: String,
    pub commit: String,
    pub sha256: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JournalRecord {
    pub action: String,
    pub commit: Option<String>,
    pub details: serde_json::Value,
    pub created_at: String,
}
