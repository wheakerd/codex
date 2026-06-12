use std::path::PathBuf;
use std::process::Command;

use crate::DesktopDistributionError;
use crate::LocatedDistribution;

pub(crate) fn from_resources_hint(resources_root: PathBuf) -> LocatedDistribution {
    let app_root = match resources_root.parent() {
        Some(parent) if parent.file_name().is_some_and(|name| name == "app") => {
            parent.parent().unwrap_or(parent).to_path_buf()
        }
        Some(parent) => parent.to_path_buf(),
        None => resources_root.clone(),
    };
    LocatedDistribution {
        app_root,
        resources_root,
    }
}

/// Queries supported MSIX identities and uses the installed package location as the app root.
pub(crate) fn discover() -> Result<LocatedDistribution, DesktopDistributionError> {
    let output = Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg(
            "$names = @('OpenAI.Codex', 'OpenAI.CodexBeta', 'OpenAI.CodexAlpha', 'OpenAI.CodexNightly'); foreach ($name in $names) { $location = Get-AppxPackage -Name $name | Select-Object -First 1 -ExpandProperty InstallLocation; if ($location) { $location; break } }",
        )
        .output()
        .map_err(|error| DesktopDistributionError::Discovery(error.to_string()))?;
    if !output.status.success() {
        return Err(DesktopDistributionError::Discovery(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let install_location = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if install_location.is_empty() {
        return Err(DesktopDistributionError::NotFound);
    }
    let app_root = PathBuf::from(install_location);
    Ok(LocatedDistribution {
        resources_root: app_root.join("app/resources"),
        app_root,
    })
}
