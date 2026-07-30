use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "aur-step")]
#[command(about = "Root-supervised, user-built AUR orchestration primitive")]
#[command(
    long_about = "Root-supervised, user-built AUR orchestration for AI agents.\n\nAUR-controlled code runs as the configured build user. Root is used only for pacman operations and trusted state management.",
    after_long_help = "Examples:\n  aur-step import-yay --json\n  sudo aur-step fetch visual-studio-code-bin\n  aur-step inspect visual-studio-code-bin --json\n  aur-step review visual-studio-code-bin\n  sudo aur-step install visual-studio-code-bin --json\n  aur-step upgrade --plan --json\n  sudo aur-step upgrade --plan --refresh --json\n  sudo aur-step upgrade --json"
)]
pub struct Cli {
    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Path to aur-step.toml.
    #[arg(long, global = true, env = "AUR_STEP_CONFIG")]
    pub config: Option<Utf8PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize state directories and the SQLite database.
    Init,
    /// Show managed package state.
    Status,
    /// Clone or fast-forward an AUR checkout as the build user.
    Fetch {
        /// AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Import installed foreign packages and matching Yay cache checkouts.
    #[command(
        after_long_help = "Examples:\n  aur-step import-yay --json\n  aur-step import-yay --include-cache-only --json"
    )]
    ImportYay {
        /// Also import Yay cache directories that are not installed foreign packages.
        #[arg(long)]
        include_cache_only: bool,
    },
    /// Show checkout paths, current commit, review state, and review diff.
    Inspect {
        /// Managed or fetchable AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Record the current checkout commit as reviewed.
    Review {
        /// Managed AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Classify dependencies from .SRCINFO.
    Deps {
        /// Walk confirmed AUR dependencies through existing local .SRCINFO files.
        #[arg(long)]
        recursive: bool,
        /// Managed or locally checked out AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Install official repo dependencies required by an AUR package.
    #[command(
        after_long_help = "Examples:\n  aur-step install-repo-deps --plan brave-bin --json\n  sudo aur-step install-repo-deps --provider ttf-font=noto-fonts brave-bin"
    )]
    InstallRepoDeps {
        /// Print the pacman plan without installing dependencies.
        #[arg(long)]
        plan: bool,
        /// Select an explicit provider candidate, for example ttf-font=noto-fonts.
        #[arg(long = "provider")]
        providers: Vec<String>,
        /// Managed AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Run makepkg as the build user.
    Build {
        /// Managed AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Install built package artifacts with pacman -U.
    InstallBuilt {
        /// Print the pacman -U plan without installing artifacts.
        #[arg(long)]
        plan: bool,
        /// Managed AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Remove generated build outputs for a package checkout.
    #[command(
        after_long_help = "Removes src/, pkg/, and built package archives. Preserves PKGBUILD, .SRCINFO, .git, signatures, and state.\n\nExample:\n  aur-step clean visual-studio-code-bin --json"
    )]
    Clean {
        /// Managed AUR package name.
        #[arg(value_name = "PACKAGE")]
        package: String,
    },
    /// Remove installed packages with pacman -Rns and clean aur-step state.
    #[command(
        after_long_help = "Examples:\n  aur-step remove --plan visual-studio-code-bin --json\n  sudo aur-step remove visual-studio-code-bin --json"
    )]
    Remove {
        /// Print the pacman -Rns plan without removing packages or state.
        #[arg(long)]
        plan: bool,
        /// Package names to remove.
        #[arg(value_name = "PACKAGE")]
        packages: Vec<String>,
    },
    /// Fetch, review-gate, dependency-install, build, and install AUR packages.
    #[command(
        after_long_help = "Examples:\n  sudo aur-step install visual-studio-code-bin --json\n  aur-step review visual-studio-code-bin\n  sudo aur-step install visual-studio-code-bin --json\n  sudo aur-step install --assume-reviewed --auto-aur-deps --provider ttf-font=noto-fonts some-package --json"
    )]
    Install {
        /// Treat fetched checkouts as reviewed for this run.
        #[arg(long)]
        assume_reviewed: bool,
        /// Recursively install confirmed AUR dependencies, preserving review gates.
        #[arg(long)]
        auto_aur_deps: bool,
        /// Select an explicit provider candidate, for example ttf-font=noto-fonts.
        #[arg(long = "provider")]
        providers: Vec<String>,
        /// AUR package names to install.
        #[arg(value_name = "PACKAGE")]
        packages: Vec<String>,
    },
    /// Plan or execute repo and reviewed AUR package upgrades.
    #[command(
        after_long_help = "Examples:\n  aur-step upgrade --plan --json\n  sudo aur-step upgrade --plan --refresh --json\n  sudo aur-step upgrade --provider ttf-font=noto-fonts --json"
    )]
    Upgrade {
        /// Plan only; do not run pacman -Syu, build, or install artifacts.
        #[arg(long)]
        plan: bool,
        /// In plan mode, refresh AUR git checkouts and .SRCINFO as the build user.
        #[arg(long)]
        refresh: bool,
        /// Select an explicit provider candidate, for example ttf-font=noto-fonts.
        #[arg(long = "provider")]
        providers: Vec<String>,
    },
}
