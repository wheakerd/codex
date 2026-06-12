use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::DesktopDistributionError;
use crate::LocatedDistribution;

const BUNDLE_IDENTIFIER: &str = "com.openai.codex";

pub(crate) fn from_resources_hint(resources_root: PathBuf) -> LocatedDistribution {
    let app_root = resources_root
        .ancestors()
        .find(|ancestor| ancestor.extension().is_some_and(|value| value == "app"))
        .map_or_else(|| resources_root.clone(), Path::to_path_buf);
    LocatedDistribution {
        app_root,
        resources_root,
    }
}

/// Asks LaunchServices for the installed stable Codex app, independent of name or location.
pub(crate) fn discover() -> Result<LocatedDistribution, DesktopDistributionError> {
    let script = format!(r#"POSIX path of (path to application id "{BUNDLE_IDENTIFIER}")"#);
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .map_err(|error| DesktopDistributionError::Discovery(error.to_string()))?;
    if !output.status.success() {
        return Err(DesktopDistributionError::NotFound);
    }
    let app_root = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let resources_root = app_root.join("Contents/Resources");
    if app_root.extension().is_some_and(|value| value == "app") && resources_root.is_dir() {
        return Ok(LocatedDistribution {
            app_root,
            resources_root,
        });
    }
    Err(DesktopDistributionError::NotFound)
}
