use crate::fs_safety;
use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use nix::unistd::{Uid, User};
use serde::Deserialize;
use std::env;

const DEFAULT_STATE_DB: &str = "/var/lib/aur-step/state.sqlite3";
const DEFAULT_AUR_URL: &str = "https://aur.archlinux.org";
const DEFAULT_CONFIG: &str = "/etc/aur-step.toml";

#[derive(Debug, Clone)]
pub struct Config {
    pub build_user: String,
    pub build_root: Utf8PathBuf,
    pub state_db: Utf8PathBuf,
    pub yay_build_dir: Utf8PathBuf,
    pub aur_url: String,
    pub sandbox_builds: bool,
    pub allow_build_network: bool,
    pub sandbox_home: Utf8PathBuf,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    build_user: Option<String>,
    build_root: Option<Utf8PathBuf>,
    state_db: Option<Utf8PathBuf>,
    yay_build_dir: Option<Utf8PathBuf>,
    aur_url: Option<String>,
    sandbox_builds: Option<bool>,
    allow_build_network: Option<bool>,
    sandbox_home: Option<Utf8PathBuf>,
}

impl Config {
    pub fn load(path: Option<&Utf8Path>) -> Result<Self> {
        let default_path = Utf8Path::new(DEFAULT_CONFIG);
        let selected_path = path.or_else(|| {
            default_path
                .try_exists()
                .ok()
                .filter(|exists| *exists)
                .map(|_| default_path)
        });
        let raw = if let Some(path) = selected_path {
            let text = fs_safety::read_config_text(path)?;
            toml::from_str(&text).with_context(|| format!("failed to parse {path}"))?
        } else {
            ConfigFile::default()
        };

        let build_user = match raw.build_user {
            Some(user) => user,
            None => infer_build_user()?,
        };
        let user = User::from_name(&build_user)
            .with_context(|| format!("failed to look up build user {build_user}"))?
            .ok_or_else(|| anyhow::anyhow!("build user {build_user} does not exist"))?;
        if user.uid == Uid::from_raw(0) {
            bail!("refusing to use root as the AUR build user");
        }
        let home = Utf8PathBuf::from_path_buf(user.dir)
            .map_err(|path| anyhow::anyhow!("build user home path is not UTF-8: {path:?}"))?;
        let build_root = raw.build_root.unwrap_or_else(|| home.join("aurbuild"));
        let config = Self {
            build_user,
            sandbox_home: raw
                .sandbox_home
                .unwrap_or_else(|| build_root.join(".aur-step-home")),
            build_root,
            state_db: raw
                .state_db
                .unwrap_or_else(|| Utf8PathBuf::from(DEFAULT_STATE_DB)),
            yay_build_dir: raw.yay_build_dir.unwrap_or_else(|| home.join(".cache/yay")),
            aur_url: raw.aur_url.unwrap_or_else(|| DEFAULT_AUR_URL.to_owned()),
            sandbox_builds: raw.sandbox_builds.unwrap_or(true),
            allow_build_network: raw.allow_build_network.unwrap_or(false),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn package_dir(&self, package: &str) -> Utf8PathBuf {
        self.build_root.join(package)
    }

    fn validate(&self) -> Result<()> {
        fs_safety::validate_absolute_normalized(&self.build_root, "build_root")?;
        fs_safety::validate_absolute_normalized(&self.state_db, "state_db")?;
        fs_safety::validate_absolute_normalized(&self.yay_build_dir, "yay_build_dir")?;
        fs_safety::validate_absolute_normalized(&self.sandbox_home, "sandbox_home")?;
        if self.sandbox_home.parent() != Some(self.build_root.as_path()) {
            bail!("sandbox_home must be a direct child of build_root");
        }
        if self.aur_url.trim().is_empty() {
            bail!("aur_url must not be empty");
        }
        Ok(())
    }
}

fn infer_build_user() -> Result<String> {
    for variable in ["AUR_STEP_BUILD_USER", "SUDO_USER"] {
        if let Ok(user) = env::var(variable) {
            let user = user.trim();
            if !user.is_empty() && user != "root" {
                return Ok(user.to_owned());
            }
        }
    }
    if !Uid::effective().is_root() {
        let user = User::from_uid(Uid::effective())
            .context("failed to look up current user")?
            .ok_or_else(|| anyhow::anyhow!("current user does not exist in the user database"))?;
        return Ok(user.name);
    }
    bail!(
        "build_user is required; set it in {DEFAULT_CONFIG}, pass --config, or set AUR_STEP_BUILD_USER"
    )
}
