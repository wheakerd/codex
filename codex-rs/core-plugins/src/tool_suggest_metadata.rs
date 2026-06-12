use std::collections::HashMap;
use std::sync::RwLock;

use codex_plugin::PluginCapabilitySummary;
use codex_plugin::PluginId;
use codex_plugin::PluginIdError;
use codex_plugin::prompt_safe_plugin_description;
use codex_protocol::protocol::Product;
use tokio::sync::Semaphore;

use crate::loader::PluginCapabilitySkillMode;
use crate::loader::load_plugin_capability_summary;
use crate::manager::ConfiguredMarketplacePlugin;
use crate::manager::remote_plugin_install_required_description;
use crate::marketplace::MarketplaceError;
use crate::marketplace::MarketplacePluginSource;

const MAX_TOOL_SUGGEST_METADATA_ENTRIES: usize = 1024;

type ToolSuggestMetadataEntry = Result<PluginCapabilitySummary, String>;

/// Materialized source-derived plugin metadata for tool suggestions.
///
/// Eligibility inputs remain live in `discoverable`; source-change invalidation clears these
/// entries so marketplace updates cannot leave stale suggestions behind.
pub(crate) struct ToolSuggestMetadataCatalog {
    state: RwLock<ToolSuggestMetadataCatalogState>,
    load_semaphore: Semaphore,
}

#[derive(Default)]
struct ToolSuggestMetadataCatalogState {
    generation: u64,
    entries: HashMap<PluginArtifactIdentity, ToolSuggestMetadataEntry>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PluginArtifactIdentity {
    plugin_id: String,
    source: MarketplacePluginSource,
}

impl ToolSuggestMetadataCatalog {
    pub(crate) fn new() -> Self {
        Self {
            state: RwLock::new(ToolSuggestMetadataCatalogState::default()),
            load_semaphore: Semaphore::new(/*permits*/ 1),
        }
    }

    pub(crate) fn clear(&self) {
        let mut state = match self.state.write() {
            Ok(state) => state,
            Err(err) => err.into_inner(),
        };
        state.generation = state.generation.wrapping_add(1);
        state.entries.clear();
    }

    pub(crate) async fn metadata_for_plugin(
        &self,
        marketplace_name: &str,
        plugin: &ConfiguredMarketplacePlugin,
        restriction_product: Option<Product>,
    ) -> Result<PluginCapabilitySummary, MarketplaceError> {
        let artifact = PluginArtifactIdentity {
            plugin_id: plugin.id.clone(),
            source: plugin.source.clone(),
        };
        loop {
            if let Some(entry) = self.cached_entry(&artifact) {
                return entry.map_err(MarketplaceError::InvalidPlugin);
            }

            let _load_permit = self.load_semaphore.acquire().await.map_err(|_| {
                MarketplaceError::InvalidPlugin(
                    "tool-suggest metadata catalog loader closed".to_string(),
                )
            })?;
            if let Some(entry) = self.cached_entry(&artifact) {
                return entry.map_err(MarketplaceError::InvalidPlugin);
            }

            let generation = self.generation();
            let entry = load_plugin_metadata(marketplace_name, plugin, restriction_product).await;
            if self.cache_entry_if_current(generation, artifact.clone(), entry.clone()) {
                return entry.map_err(MarketplaceError::InvalidPlugin);
            }
        }
    }

    fn cached_entry(&self, artifact: &PluginArtifactIdentity) -> Option<ToolSuggestMetadataEntry> {
        match self.state.read() {
            Ok(state) => state.entries.get(artifact).cloned(),
            Err(err) => err.into_inner().entries.get(artifact).cloned(),
        }
    }

    fn generation(&self) -> u64 {
        match self.state.read() {
            Ok(state) => state.generation,
            Err(err) => err.into_inner().generation,
        }
    }

    fn cache_entry_if_current(
        &self,
        generation: u64,
        artifact: PluginArtifactIdentity,
        entry: ToolSuggestMetadataEntry,
    ) -> bool {
        let mut state = match self.state.write() {
            Ok(state) => state,
            Err(err) => err.into_inner(),
        };
        if state.generation == generation {
            if state.entries.len() >= MAX_TOOL_SUGGEST_METADATA_ENTRIES
                && !state.entries.contains_key(&artifact)
            {
                state.entries.clear();
            }
            state.entries.insert(artifact, entry);
            true
        } else {
            false
        }
    }
}

async fn load_plugin_metadata(
    marketplace_name: &str,
    plugin: &ConfiguredMarketplacePlugin,
    restriction_product: Option<Product>,
) -> ToolSuggestMetadataEntry {
    let plugin_id = PluginId::new(plugin.name.clone(), marketplace_name.to_string()).map_err(
        |err| match err {
            PluginIdError::Invalid(message) => message,
        },
    )?;

    let MarketplacePluginSource::Local { path: plugin_root } = &plugin.source else {
        return Ok(PluginCapabilitySummary {
            config_name: plugin.id.clone(),
            display_name: plugin.name.clone(),
            description: prompt_safe_plugin_description(Some(
                &remote_plugin_install_required_description(&plugin.source),
            )),
            ..PluginCapabilitySummary::default()
        });
    };
    if !plugin_root.as_path().is_dir() {
        return Err("path does not exist or is not a directory".to_string());
    }
    let mut summary = load_plugin_capability_summary(
        &plugin_id,
        plugin_root,
        PluginCapabilitySkillMode::ValidForProduct(restriction_product),
    )
    .await
    .ok_or_else(|| "missing or invalid plugin.json".to_string())?;
    summary.config_name = plugin.id.clone();
    summary.display_name = plugin.name.clone();
    Ok(summary)
}
