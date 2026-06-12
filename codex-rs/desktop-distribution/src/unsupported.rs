use std::path::PathBuf;

use crate::DesktopDistributionError;
use crate::LocatedDistribution;

pub(crate) fn from_resources_hint(resources_root: PathBuf) -> LocatedDistribution {
    LocatedDistribution {
        app_root: resources_root.clone(),
        resources_root,
    }
}

pub(crate) fn discover() -> Result<LocatedDistribution, DesktopDistributionError> {
    Err(DesktopDistributionError::Unsupported)
}
