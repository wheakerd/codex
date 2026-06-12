use super::*;
use crate::app_bundled_internal::load_app_bundled_internal_hooks_from_distribution;
use crate::manifest::load_plugin_manifest;
use crate::test_support::write_file;
use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigRequirements;
use codex_config::ConfigRequirementsToml;
use codex_config::HookHandlerConfig;
use codex_desktop_distribution::DesktopDistribution;
use codex_plugin::AppConnectorId;
use codex_plugin::PluginHookSourceKind;
use codex_plugin::PluginId;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn user_config_path(temp_dir: &TempDir, file_name: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(temp_dir.path().join(file_name))
        .expect("test user config path should be absolute")
}

fn user_layer(path: AbsolutePathBuf, config: &str) -> ConfigLayerEntry {
    ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: path,
            profile: None,
        },
        toml::from_str(config).expect("user config toml"),
    )
}

#[test]
fn configured_plugins_from_stack_merges_user_layers() {
    let temp_dir = TempDir::new().expect("tempdir");
    let stack = ConfigLayerStack::new(
        vec![
            user_layer(
                user_config_path(&temp_dir, "config.toml"),
                "[plugins.base]\nenabled = true\n",
            ),
            user_layer(
                user_config_path(&temp_dir, "work.config.toml"),
                "[plugins.profile]\nenabled = false\n",
            ),
        ],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("valid config layer stack");

    let plugins = configured_plugins_from_stack(&stack);

    assert_eq!(
        plugins,
        HashMap::from([
            (
                "base".to_string(),
                PluginConfig {
                    enabled: true,
                    mcp_servers: HashMap::new(),
                },
            ),
            (
                "profile".to_string(),
                PluginConfig {
                    enabled: false,
                    mcp_servers: HashMap::new(),
                },
            ),
        ])
    );
}

#[test]
fn app_bundled_discovery_failure_never_loads_cached_hooks() {
    let plugin_id = PluginId::parse("computer-use@openai-bundled").expect("plugin id");

    let (sources, warnings) = select_app_bundled_hook_sources(
        &plugin_id,
        Err("distribution discovery failed".to_string()),
    );

    assert!(sources.is_empty());
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn app_bundled_hooks_use_desktop_while_other_capabilities_use_cache() {
    let temp_dir = TempDir::new().expect("tempdir");
    let plugin_id = PluginId::parse("computer-use@openai-bundled").expect("plugin id");
    let cache_root = temp_dir
        .path()
        .join("plugins/cache/openai-bundled/computer-use/local");
    write_file(
        &cache_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"computer-use","description":"cached capabilities"}"#,
    );
    write_file(
        &cache_root.join("skills/cached/SKILL.md"),
        "---\nname: cached\ndescription: cached skill\n---\n",
    );
    write_file(
        &cache_root.join(".mcp.json"),
        r#"{"mcpServers":{"cached-mcp":{"command":"echo"}}}"#,
    );
    write_file(
        &cache_root.join(".app.json"),
        r#"{"apps":{"cached":{"id":"connector_cached"}}}"#,
    );
    write_file(
        &cache_root.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"cache-spoof"}]}]}}"#,
    );

    let resources_root = temp_dir.path().join("desktop-resources");
    let bundled_root = resources_root.join("plugins/openai-bundled/plugins/computer-use");
    write_file(
        &bundled_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"computer-use","description":"bundled hooks"}"#,
    );
    let bundled_hooks_path = bundled_root.join("hooks/hooks.json");
    write_file(
        &bundled_hooks_path,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"bundle-hook"}]}]}}"#,
    );
    write_file(
        &resources_root.join("plugins/openai-bundled/.agents/plugins/marketplace.json"),
        r#"{"name":"openai-bundled","plugins":[{"name":"computer-use","source":{"source":"local","path":"./plugins/computer-use"}}]}"#,
    );
    let distribution = DesktopDistribution::from_trusted_resources_path(resources_root)
        .expect("Desktop distribution");
    let expected_bundled_root = AbsolutePathBuf::try_from(
        distribution
            .contained_directory("plugins/openai-bundled/plugins/computer-use")
            .expect("bundled plugin root"),
    )
    .expect("absolute bundled plugin root");
    let store = PluginStore::new(temp_dir.path().to_path_buf());
    let config = PluginConfig {
        enabled: true,
        mcp_servers: HashMap::new(),
    };
    let skill_config_rules = SkillConfigRules::default();
    let scope = PluginLoadScope::AllCapabilities {
        restriction_product: Some(Product::Codex),
        skill_config_rules: &skill_config_rules,
    };

    let bundled_hooks = load_app_bundled_internal_hooks_from_distribution(
        &distribution,
        &plugin_id,
        &store.plugin_data_root(&plugin_id),
    );
    let loaded = load_plugin(
        plugin_id.as_key().to_string(),
        &config,
        &store,
        &scope,
        Some(bundled_hooks),
    )
    .await;

    assert_eq!(loaded.error, None);
    assert_eq!(loaded.root.as_path(), cache_root);
    assert!(loaded.has_enabled_skills);
    assert_eq!(loaded.skill_roots, vec![loaded.root.join("skills")]);
    assert_eq!(loaded.mcp_servers.len(), 1);
    assert!(loaded.mcp_servers.contains_key("cached-mcp"));
    assert_eq!(loaded.apps, vec![AppConnectorId("connector_cached".into())]);
    let [source] = loaded.hook_sources.as_slice() else {
        panic!("expected one bundled hook source");
    };
    assert_eq!(source.kind, PluginHookSourceKind::AppBundledInternal);
    assert_eq!(source.plugin_root, expected_bundled_root);
    assert_eq!(source.plugin_data_root, store.plugin_data_root(&plugin_id));
    let [group] = source.hooks.session_start.as_slice() else {
        panic!("expected one SessionStart group");
    };
    let [HookHandlerConfig::Command { command, .. }] = group.hooks.as_slice() else {
        panic!("expected one command hook");
    };
    assert_eq!(command, "bundle-hook");

    fs::remove_file(bundled_hooks_path).expect("remove bundled hooks");
    let bundled_hooks = load_app_bundled_internal_hooks_from_distribution(
        &distribution,
        &plugin_id,
        &store.plugin_data_root(&plugin_id),
    );
    let without_bundled_hooks = load_plugin(
        plugin_id.as_key().to_string(),
        &config,
        &store,
        &scope,
        Some(bundled_hooks),
    )
    .await;
    assert!(without_bundled_hooks.hook_sources.is_empty());
}

#[tokio::test]
async fn hooks_only_scope_shares_plugin_resolution_without_loading_other_capabilities() {
    let temp_dir = TempDir::new().expect("tempdir");
    let plugin_root = temp_dir.path().join("plugins/cache/test/valid/local");
    write_file(
        &plugin_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"valid"}"#,
    );
    write_file(
        &plugin_root.join("skills/example/SKILL.md"),
        "---\nname: example\ndescription: example skill\n---\n",
    );
    write_file(
        &plugin_root.join(".mcp.json"),
        r#"{"mcpServers":{"example":{"command":"echo"}}}"#,
    );
    write_file(
        &plugin_root.join(".app.json"),
        r#"{"apps":{"example":{"id":"connector_example"}}}"#,
    );
    write_file(
        &plugin_root.join("hooks/hooks.json"),
        r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "echo startup"
          }
        ]
      }
    ]
  }
}"#,
    );

    let disabled_root = temp_dir.path().join("plugins/cache/test/disabled/local");
    write_file(
        &disabled_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"disabled"}"#,
    );
    write_file(
        &disabled_root.join("hooks/hooks.json"),
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo disabled"}]}]}}"#,
    );

    let malformed_root = temp_dir.path().join("plugins/cache/test/malformed/local");
    write_file(
        &malformed_root.join(".codex-plugin/plugin.json"),
        "not valid json",
    );

    let warning_root = temp_dir.path().join("plugins/cache/test/warning/local");
    write_file(
        &warning_root.join(".codex-plugin/plugin.json"),
        r#"{"name":"warning"}"#,
    );
    write_file(&warning_root.join("hooks/hooks.json"), "not valid json");

    let stack = ConfigLayerStack::new(
        vec![user_layer(
            user_config_path(&temp_dir, "config.toml"),
            r#"
[plugins."valid@test"]
enabled = true

[plugins."disabled@test"]
enabled = false

[plugins.invalid]
enabled = true

[plugins."malformed@test"]
enabled = true

[plugins."missing@test"]
enabled = true

[plugins."warning@test"]
enabled = true
"#,
        )],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("valid config layer stack");
    let store = PluginStore::new(temp_dir.path().to_path_buf());

    let full = load_plugins_from_layer_stack(
        &stack,
        HashMap::new(),
        &store,
        Some(Product::Codex),
        /*prefer_remote_curated_conflicts*/ false,
    )
    .await;
    let hooks_only = load_plugins_from_layer_stack_with_scope(
        &stack,
        HashMap::new(),
        &store,
        /*prefer_remote_curated_conflicts*/ false,
        PluginLoadScope::HooksOnly,
    )
    .await;

    let validation_state = |outcome: &PluginLoadOutcome<McpServerConfig>| {
        outcome
            .plugins()
            .iter()
            .map(|plugin| {
                (
                    plugin.config_name.clone(),
                    plugin.enabled,
                    plugin.root.clone(),
                    plugin.error.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(validation_state(&hooks_only), validation_state(&full));
    assert_eq!(
        hooks_only.effective_plugin_hook_sources(),
        full.effective_plugin_hook_sources()
    );
    assert_eq!(
        hooks_only.effective_plugin_hook_warnings(),
        full.effective_plugin_hook_warnings()
    );

    let full_valid = full
        .plugins()
        .iter()
        .find(|plugin| plugin.config_name == "valid@test")
        .expect("full load should include valid plugin");
    assert!(full_valid.manifest_name.is_some());
    assert!(!full_valid.skill_roots.is_empty());
    assert!(!full_valid.mcp_servers.is_empty());
    assert!(!full_valid.apps.is_empty());

    let hooks_only_valid = hooks_only
        .plugins()
        .iter()
        .find(|plugin| plugin.config_name == "valid@test")
        .expect("hooks-only load should include valid plugin");
    assert_eq!(hooks_only_valid.manifest_name, None);
    assert!(hooks_only_valid.skill_roots.is_empty());
    assert!(hooks_only_valid.mcp_servers.is_empty());
    assert!(hooks_only_valid.apps.is_empty());
}

#[test]
fn curated_plugin_cache_version_shortens_full_git_sha() {
    assert_eq!(
        curated_plugin_cache_version("0123456789abcdef0123456789abcdef01234567"),
        "01234567"
    );
}

#[test]
fn curated_plugin_cache_version_preserves_non_git_sha_versions() {
    assert_eq!(
        curated_plugin_cache_version("export-backup"),
        "export-backup"
    );
    assert_eq!(curated_plugin_cache_version("0123456"), "0123456");
}

fn plugin_id() -> PluginId {
    PluginId::parse("demo-plugin@test-marketplace").expect("plugin id")
}

fn plugin_root() -> (tempfile::TempDir, AbsolutePathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugin_root =
        AbsolutePathBuf::try_from(tmp.path().join("demo-plugin")).expect("plugin root");
    fs::create_dir_all(plugin_root.join(".codex-plugin")).expect("create manifest dir");
    fs::create_dir_all(plugin_root.join("hooks")).expect("create hooks dir");
    (tmp, plugin_root)
}

fn write_manifest(plugin_root: &AbsolutePathBuf, manifest: &str) {
    fs::write(plugin_root.join(".codex-plugin/plugin.json"), manifest).expect("write manifest");
}

fn write_hook_file(plugin_root: &AbsolutePathBuf, relative_path: &str, event: &str, command: &str) {
    fs::write(
        plugin_root.join(relative_path),
        format!(
            r#"{{
  "hooks": {{
    "{event}": [
      {{
        "hooks": [{{ "type": "command", "command": "{command}" }}]
      }}
    ]
  }}
}}"#
        ),
    )
    .expect("write hooks");
}

fn load_sources(plugin_root: &AbsolutePathBuf) -> (Vec<PluginHookSource>, Vec<String>) {
    let manifest = load_plugin_manifest(plugin_root.as_path()).expect("manifest");
    let plugin_data_root = AbsolutePathBuf::try_from(
        plugin_root
            .as_path()
            .parent()
            .expect("plugin root parent")
            .join("plugin-data"),
    )
    .expect("plugin data root");
    load_plugin_hooks(
        plugin_root,
        &plugin_id(),
        &plugin_data_root,
        &manifest.paths,
    )
}

fn assert_sources(sources: &[PluginHookSource], expected_relative_paths: &[&str]) {
    assert_eq!(
        sources
            .iter()
            .map(|source| source.plugin_id.clone())
            .collect::<Vec<_>>(),
        vec![plugin_id(); expected_relative_paths.len()]
    );
    assert_eq!(
        sources
            .iter()
            .map(|source| source.source_relative_path.as_str())
            .collect::<Vec<_>>(),
        expected_relative_paths
    );
    assert_eq!(
        sources
            .iter()
            .map(|source| source.hooks.handler_count())
            .collect::<Vec<_>>(),
        vec![1; expected_relative_paths.len()]
    );
}

#[test]
fn load_plugin_hooks_discovers_default_hooks_file() {
    let (_tmp, plugin_root) = plugin_root();
    write_manifest(&plugin_root, r#"{ "name": "demo-plugin" }"#);
    fs::write(
        plugin_root.join("hooks/hooks.json"),
        r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "echo default" }]
      }
    ]
  }
}"#,
    )
    .expect("write hooks");

    let (sources, warnings) = load_sources(&plugin_root);

    assert_eq!(warnings, Vec::<String>::new());
    assert_sources(&sources, &["hooks/hooks.json"]);
}

#[test]
fn load_plugin_hooks_supports_manifest_hook_path() {
    let (_tmp, plugin_root) = plugin_root();
    write_manifest(
        &plugin_root,
        r#"{
  "name": "demo-plugin",
  "hooks": "./hooks/one.json"
}"#,
    );
    write_hook_file(&plugin_root, "hooks/one.json", "PreToolUse", "echo one");

    let (sources, warnings) = load_sources(&plugin_root);

    assert_eq!(warnings, Vec::<String>::new());
    assert_sources(&sources, &["hooks/one.json"]);
}

#[test]
fn load_plugin_hooks_manifest_paths_replace_default_hooks_file() {
    let (_tmp, plugin_root) = plugin_root();
    write_manifest(
        &plugin_root,
        r#"{
  "name": "demo-plugin",
  "hooks": ["./hooks/one.json", "./hooks/two.json"]
}"#,
    );
    write_hook_file(
        &plugin_root,
        "hooks/hooks.json",
        "PreToolUse",
        "echo ignored",
    );
    write_hook_file(&plugin_root, "hooks/one.json", "PreToolUse", "echo one");
    write_hook_file(&plugin_root, "hooks/two.json", "PostToolUse", "echo two");

    let (sources, warnings) = load_sources(&plugin_root);

    assert_eq!(warnings, Vec::<String>::new());
    assert_sources(&sources, &["hooks/one.json", "hooks/two.json"]);
}

#[test]
fn load_plugin_hooks_supports_inline_manifest_hooks() {
    let (_tmp, plugin_root) = plugin_root();
    write_manifest(
        &plugin_root,
        r#"{
  "name": "demo-plugin",
  "hooks": {
    "hooks": {
      "SessionStart": [
        {
          "matcher": "startup",
          "hooks": [{ "type": "command", "command": "echo inline" }]
        }
      ]
    }
  }
}"#,
    );

    let (sources, warnings) = load_sources(&plugin_root);

    assert_eq!(warnings, Vec::<String>::new());
    assert_sources(&sources, &["plugin.json#hooks[0]"]);
}

#[test]
fn load_plugin_hooks_reports_invalid_hook_file() {
    let (_tmp, plugin_root) = plugin_root();
    write_manifest(&plugin_root, r#"{ "name": "demo-plugin" }"#);
    fs::write(plugin_root.join("hooks/hooks.json"), "{ not-json").expect("write invalid hooks");

    let (sources, warnings) = load_sources(&plugin_root);

    assert_eq!(sources, Vec::<PluginHookSource>::new());
    assert_eq!(
        warnings,
        vec![format!(
            "failed to parse plugin hooks config {}: key must be a string at line 1 column 3",
            plugin_root.join("hooks/hooks.json").display()
        )]
    );
}

#[test]
fn load_plugin_hooks_supports_inline_manifest_hook_list() {
    let (_tmp, plugin_root) = plugin_root();
    write_manifest(
        &plugin_root,
        r#"{
  "name": "demo-plugin",
  "hooks": [
    {
      "hooks": {
        "SessionStart": [
          {
            "hooks": [{ "type": "command", "command": "echo inline one" }]
          }
        ]
      }
    },
    {
      "hooks": {
        "Stop": [
          {
            "hooks": [{ "type": "command", "command": "echo inline two" }]
          }
        ]
      }
    }
  ]
}"#,
    );

    let (sources, warnings) = load_sources(&plugin_root);

    assert_eq!(warnings, Vec::<String>::new());
    assert_sources(&sources, &["plugin.json#hooks[0]", "plugin.json#hooks[1]"]);
}

#[test]
fn materialize_git_subdir_uses_sparse_checkout() {
    let codex_home = tempfile::tempdir().expect("create codex home");
    let repo = tempfile::tempdir().expect("create git repo");
    let plugin_dir = repo.path().join("plugins/toolkit");
    fs::create_dir_all(&plugin_dir).expect("create plugin directory");
    fs::create_dir_all(repo.path().join("plugins/other")).expect("create other plugin");
    fs::write(plugin_dir.join("marker.txt"), "toolkit").expect("write plugin marker");
    fs::write(repo.path().join("plugins/other/marker.txt"), "other").expect("write other marker");
    fs::write(repo.path().join("root.txt"), "root").expect("write root marker");

    run_git(&["init"], Some(repo.path())).expect("init git repo");
    run_git(
        &["config", "user.email", "test@example.com"],
        Some(repo.path()),
    )
    .expect("configure git email");
    run_git(&["config", "user.name", "Test User"], Some(repo.path())).expect("configure git name");
    run_git(&["add", "."], Some(repo.path())).expect("stage git repo");
    run_git(&["commit", "-m", "init"], Some(repo.path())).expect("commit git repo");

    let materialized = materialize_marketplace_plugin_source(
        codex_home.path(),
        &MarketplacePluginSource::Git {
            url: repo.path().display().to_string(),
            path: Some("plugins/toolkit".to_string()),
            ref_name: None,
            sha: None,
        },
    )
    .expect("materialize git source");

    assert_eq!(
        plugin_dir.file_name(),
        materialized.path.as_path().file_name()
    );
    assert!(materialized.path.as_path().join("marker.txt").is_file());
    let checkout_root = materialized
        .path
        .as_path()
        .parent()
        .and_then(Path::parent)
        .expect("materialized path should be nested under checkout root");
    assert!(!checkout_root.join("root.txt").exists());
    assert!(!checkout_root.join("plugins/other/marker.txt").exists());
}
