//! Locates the trusted Codex Desktop resources bundled with the installed app.

use std::fs;
use std::io;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;
#[cfg(not(any(target_os = "macos", windows)))]
#[path = "unsupported.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

pub const DESKTOP_RESOURCES_PATH_ENV_VAR: &str = "CODEX_DESKTOP_RESOURCES_PATH";

#[derive(Debug, thiserror::Error)]
pub enum DesktopDistributionError {
    /// This platform has no supported Desktop distribution discovery mechanism.
    #[error("Codex Desktop distribution discovery is unsupported on this platform")]
    Unsupported,
    /// No launcher hint was provided and no installed Codex Desktop app was found. This can
    /// happen, for example, when Desktop was uninstalled after installing a bundled plugin.
    #[error("no installed Codex Desktop distribution was found")]
    NotFound,
    /// The platform app-discovery mechanism could not be queried successfully.
    #[error("Codex Desktop discovery failed: {0}")]
    Discovery(String),
    /// An expected app resource could not be read or canonicalized.
    #[error("Codex Desktop filesystem validation failed during {stage}: {source}")]
    Filesystem {
        stage: &'static str,
        #[source]
        source: io::Error,
    },
    /// A requested resource was absolute, contained `..`, escaped through a symlink, or had the
    /// wrong file kind, so it was not strictly contained beneath Desktop's resources directory.
    #[error("Codex Desktop resource containment failed: {0}")]
    Containment(String),
}

#[derive(Debug, Clone)]
pub struct DesktopDistribution {
    app_root: PathBuf,
    resources_root: PathBuf,
}

impl DesktopDistribution {
    pub fn app_root(&self) -> &Path {
        &self.app_root
    }

    pub fn resources_root(&self) -> &Path {
        &self.resources_root
    }

    pub fn contained_file(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<PathBuf, DesktopDistributionError> {
        contained_path(
            &self.resources_root,
            relative_path.as_ref(),
            ResourceKind::File,
        )
    }

    pub fn contained_directory(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<PathBuf, DesktopDistributionError> {
        contained_path(
            &self.resources_root,
            relative_path.as_ref(),
            ResourceKind::Directory,
        )
    }

    fn new(app_root: PathBuf, resources_root: PathBuf) -> Result<Self, DesktopDistributionError> {
        let app_root = canonical(&app_root, "application root")?;
        let resources_root = canonical(&resources_root, "resources root")?;
        if resources_root != app_root && !resources_root.starts_with(&app_root) {
            return Err(containment(
                "resources root is outside the discovered application root",
            ));
        }
        if !resources_root.is_dir() {
            return Err(containment("expected the resources root to be a directory"));
        }
        Ok(Self {
            app_root,
            resources_root,
        })
    }

    /// Uses a resources directory supplied by the trusted Desktop launcher.
    pub fn from_trusted_resources_path(
        resources_root: PathBuf,
    ) -> Result<Self, DesktopDistributionError> {
        let located = platform::from_resources_hint(resources_root);
        Self::new(located.app_root, located.resources_root)
    }
}

/// Uses Desktop's explicit resources hint when present, otherwise discovers an installed app.
pub fn locate_current_or_installed_distribution()
-> Result<DesktopDistribution, DesktopDistributionError> {
    if let Some(resources_root) = std::env::var_os(DESKTOP_RESOURCES_PATH_ENV_VAR) {
        return DesktopDistribution::from_trusted_resources_path(PathBuf::from(resources_root));
    }
    let located = platform::discover()?;
    DesktopDistribution::new(located.app_root, located.resources_root)
}

pub(crate) struct LocatedDistribution {
    pub app_root: PathBuf,
    pub resources_root: PathBuf,
}

#[derive(Clone, Copy)]
enum ResourceKind {
    Directory,
    File,
}

fn contained_path(
    root: &Path,
    relative_path: &Path,
    kind: ResourceKind,
) -> Result<PathBuf, DesktopDistributionError> {
    if relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(containment(
            "resource paths must contain only normal relative components",
        ));
    }
    let path = canonical(&root.join(relative_path), "resource path")?;
    if path == root || !path.starts_with(root) {
        return Err(containment(
            "resource must remain strictly below the Desktop resources root",
        ));
    }
    let metadata = fs::metadata(&path).map_err(|source| DesktopDistributionError::Filesystem {
        stage: "resource metadata",
        source,
    })?;
    let expected_kind = match kind {
        ResourceKind::Directory => metadata.is_dir(),
        ResourceKind::File => metadata.is_file(),
    };
    if !expected_kind {
        return Err(containment(match kind {
            ResourceKind::Directory => "expected a directory",
            ResourceKind::File => "expected a regular file",
        }));
    }
    Ok(path)
}

fn canonical(path: &Path, stage: &'static str) -> Result<PathBuf, DesktopDistributionError> {
    dunce::canonicalize(path)
        .map_err(|source| DesktopDistributionError::Filesystem { stage, source })
}

fn containment(message: impl Into<String>) -> DesktopDistributionError {
    DesktopDistributionError::Containment(message.into())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
