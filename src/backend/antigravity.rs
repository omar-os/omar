//! Antigravity: registered as an MCP plugin in the operator's config and
//! driven over its protocol by OMAR's runner.
use std::path::PathBuf;

use anyhow::Result;

use super::{managed, spool, PaneSetup};
use super::{Backend, Kind, Launch};
use crate::ea::EaId;
use crate::manager::{
    managed_agent_command, materialize_prompt_file, materialize_shared_mcp_context_file,
    omar_server_exe, shell_single_quote, write_private_file, McpLaunchContext,
};

pub struct Antigravity;

impl Backend for Antigravity {
    fn kind(&self) -> Kind {
        Kind::Antigravity
    }
    fn aliases(&self) -> &'static [&'static str] {
        &["agy", "antigravity"]
    }
    fn executables(&self) -> &'static [&'static str] {
        &["agy"]
    }
    fn default_command(&self) -> &'static str {
        "agy --dangerously-skip-permissions"
    }
    // No readiness banner is pinned: `agy` is not available in CI or local
    // discovery, so a marker would be an unverified guess.
    /// Antigravity takes no message from outside its protocol, so it runs
    /// under OMAR's protocol runner, once its MCP registration is written.
    fn launch_command(&self, launch: &Launch<'_>) -> String {
        let rendered = materialize_prompt_file(launch.prompt_file, launch.substitutions);
        if ensure_antigravity_mcp_config(launch.context).is_none() {
            return "printf '%s\n' 'OMAR cannot configure Antigravity MCP' >&2; exit 1".into();
        }
        managed_agent_command("agy", launch.base_command, &rendered, launch.context, None)
            .unwrap_or_else(|error| {
                format!(
                    "printf '%s\n' {} >&2; exit 1",
                    shell_single_quote(&format!("OMAR protocol launch failed: {error:#}"))
                )
            })
    }

    /// Antigravity takes no message from outside its protocol. Under the runner
    /// nothing else is needed; a legacy launch gets its hook installed and a
    /// spool the hook drains, pointed at by the pane's environment.
    fn prepare_pane(&self, session: &str, command: &str) -> Result<PaneSetup> {
        let mut setup = PaneSetup {
            command: command.to_string(),
            ..PaneSetup::default()
        };
        if managed::managed_launch_socket(command).is_none() && install_antigravity_hook() {
            spool::reset_spool(session);
            let path = spool::spool_path(session);
            // Quoted: a space anywhere in the path would otherwise split the
            // assignment and take the rest of the launch line with it.
            setup.command = format!(
                "OMAR_EVENT_SPOOL={} {}",
                shell_single_quote(&path.display().to_string()),
                command
            );
            setup.stamp = Some(spool::stamp(&path));
        }
        Ok(setup)
    }
    /// The hook only runs once the agent is already working, so a queued
    /// event cannot wake an idle one.
    fn spool_wakes_idle(&self) -> bool {
        false
    }
    fn hook_reply(&self, events: &[String]) -> Option<String> {
        Some(if events.is_empty() {
            "{}".to_string()
        } else {
            serde_json::json!({ "injectSteps": [{ "ephemeralMessage": events.join("\n\n") }] })
                .to_string()
        })
    }
}

pub(crate) fn ensure_antigravity_mcp_config(context: &McpLaunchContext) -> Option<()> {
    // Antigravity CLI loads MCP servers from native plugin bundles. Keep OMAR's
    // plugin EA-scoped so lifecycle and cleanup do not touch user plugins.
    // `agy plugin install` stages active plugins under ~/.gemini/config/plugins
    // and records them in ~/.gemini/config/import_manifest.json.
    let server_exe = omar_server_exe()?;
    let context_file = materialize_shared_mcp_context_file(context)?;
    let home = std::env::var("HOME").ok()?;
    let config_dir = PathBuf::from(home).join(".gemini").join("config");
    let plugins_dir = config_dir.join("plugins");
    std::fs::create_dir_all(&plugins_dir).ok()?;
    let key = format!("omar-ea-{}", context.ea_id);
    let plugin_dir = plugins_dir.join(&key);
    std::fs::create_dir_all(&plugin_dir).ok()?;

    if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path == plugin_dir {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with("omar-ea-") {
                    let ctx_path = path
                        .join("mcp_config.json")
                        .canonicalize()
                        .ok()
                        .and_then(|config| std::fs::read_to_string(config).ok())
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .and_then(|v| {
                            v.get("mcpServers")
                                .and_then(|servers| servers.get(name))
                                .and_then(|server| server.get("args"))
                                .and_then(|args| args.as_array())
                                .and_then(|args| args.last())
                                .and_then(|arg| arg.as_str())
                                .map(PathBuf::from)
                        });
                    if ctx_path.map(|p| !p.exists()).unwrap_or(false) {
                        let _ = std::fs::remove_dir_all(path);
                    }
                }
            }
        }
    }

    let plugin = serde_json::json!({
        "name": key.clone(),
        "version": "0.0.0",
        "description": "OMAR MCP server registration for this Executive Assistant"
    });
    let mut servers = serde_json::Map::new();
    servers.insert(
        key.clone(),
        serde_json::json!({
        "command": server_exe.display().to_string(),
        "args": ["mcp-server", "--context-file", context_file.display().to_string()],
        }),
    );
    let config = serde_json::json!({
        "mcpServers": servers
    });

    let plugin_path = plugin_dir.join("plugin.json");
    let config_path = plugin_dir.join("mcp_config.json");
    let manifest_path = config_dir.join("import_manifest.json");
    let plugin_payload = serde_json::to_vec_pretty(&plugin).ok()?;
    let config_payload = serde_json::to_vec_pretty(&config).ok()?;
    write_private_file(&plugin_path, &plugin_payload).ok()?;
    write_private_file(&config_path, &config_payload).ok()?;

    let mut manifest = match std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    {
        Some(v) if v.is_object() => v,
        _ => serde_json::json!({}),
    };
    let mut imports = manifest
        .get("imports")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    imports.retain(|entry| {
        let Some(name) = entry.get("name").and_then(|name| name.as_str()) else {
            return true;
        };
        if name == key {
            return false;
        }
        if let Some(ea) = name.strip_prefix("omar-ea-") {
            return plugins_dir.join(format!("omar-ea-{ea}")).exists();
        }
        true
    });
    imports.push(serde_json::json!({
        "name": key,
        "source": "local-install",
        "importedAt": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "components": ["installed"],
    }));
    manifest["imports"] = serde_json::Value::Array(imports);
    let manifest_payload = serde_json::to_vec_pretty(&manifest).ok()?;
    write_private_file(&manifest_path, &manifest_payload).ok()?;
    Some(())
}

pub(crate) fn antigravity_config_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".gemini").join("config"))
}

pub(crate) fn rewrite_antigravity_manifest_without<F>(keep_import: F) -> Result<()>
where
    F: Fn(&str) -> bool,
{
    let Some(config_dir) = antigravity_config_dir() else {
        return Ok(());
    };
    let manifest_path = config_dir.join("import_manifest.json");
    let Some(mut manifest) = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .filter(|v| v.is_object())
    else {
        return Ok(());
    };
    let Some(imports) = manifest.get("imports").and_then(|v| v.as_array()) else {
        return Ok(());
    };
    let retained: Vec<_> = imports
        .iter()
        .filter(|entry| {
            entry
                .get("name")
                .and_then(|name| name.as_str())
                .map(&keep_import)
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    manifest["imports"] = serde_json::Value::Array(retained);
    write_private_file(&manifest_path, &serde_json::to_vec_pretty(&manifest)?)?;
    Ok(())
}

pub(crate) fn remove_omar_antigravity_mcp_config(ea_id: EaId) -> Result<()> {
    let Some(config_dir) = antigravity_config_dir() else {
        return Ok(());
    };
    let key = format!("omar-ea-{ea_id}");
    let plugin_dir = config_dir.join("plugins").join(&key);
    if plugin_dir.exists() {
        std::fs::remove_dir_all(&plugin_dir)?;
    }
    rewrite_antigravity_manifest_without(|name| name != key)
}

pub(crate) fn remove_all_omar_antigravity_mcp_configs() -> Result<()> {
    let Some(config_dir) = antigravity_config_dir() else {
        return Ok(());
    };
    let plugins_dir = config_dir.join("plugins");
    if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
        for entry in entries.flatten() {
            if entry
                .file_name()
                .to_str()
                .map(|name| name.starts_with("omar-ea-"))
                .unwrap_or(false)
            {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
    }
    rewrite_antigravity_manifest_without(|name| !name.starts_with("omar-ea-"))
}

/// Install OMAR's hook so antigravity will collect events before each turn.
///
/// Hooks are a map of named hooks that the CLI merges, so OMAR claims one name
/// and leaves the rest of the file alone. `PreInvocation` runs just before the
/// model is called, which is the moment queued events are worth handing over.
///
/// The file is the shared one under `~/.gemini/config` rather than the
/// per-workspace `.agents/hooks.json`. Both are read — the workspace one on a
/// second load, once the folder is resolved and trusted — but a copy per
/// workspace would leave an untracked file in the operator's repository, and
/// one shared entry already serves every pane.
pub(crate) fn install_antigravity_hook() -> bool {
    let Some(exe) = std::env::current_exe().ok() else {
        return false;
    };
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let path = home.join(".gemini").join("config").join("hooks.json");

    let mut config: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let Some(hooks) = config.as_object_mut() else {
        return false;
    };

    hooks.insert(
        "omar".to_string(),
        serde_json::json!({
            "PreInvocation": [{
                "type": "command",
                "command": format!("{} hook-drain --format agy", crate::manager::shell_single_quote(&exe.display().to_string())),
            }]
        }),
    );

    spool::write_json_atomically(&path, &config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::global_home_env_lock;
    use crate::manager::tests::*;

    #[test]
    fn test_remove_omar_antigravity_mcp_config_updates_plugin_and_manifest() {
        let _env_lock = global_home_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", dir.path());
        let plugins_dir = dir.path().join(".gemini/config/plugins");
        std::fs::create_dir_all(plugins_dir.join("omar-ea-7")).unwrap();
        std::fs::create_dir_all(plugins_dir.join("other-plugin")).unwrap();
        let manifest_path = dir.path().join(".gemini/config/import_manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "imports": [
                    {"name": "omar-ea-7", "source": "local-install"},
                    {"name": "other-plugin", "source": "local-install"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        remove_omar_antigravity_mcp_config(7).unwrap();

        assert!(!plugins_dir.join("omar-ea-7").exists());
        assert!(plugins_dir.join("other-plugin").exists());
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        let imports = manifest["imports"].as_array().unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0]["name"], "other-plugin");
    }

    #[test]
    fn test_remove_all_omar_antigravity_mcp_configs_preserves_user_plugins() {
        let _env_lock = global_home_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", dir.path());
        let plugins_dir = dir.path().join(".gemini/config/plugins");
        std::fs::create_dir_all(plugins_dir.join("omar-ea-1")).unwrap();
        std::fs::create_dir_all(plugins_dir.join("omar-ea-2")).unwrap();
        std::fs::create_dir_all(plugins_dir.join("user-plugin")).unwrap();
        let manifest_path = dir.path().join(".gemini/config/import_manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "imports": [
                    {"name": "omar-ea-1", "source": "local-install"},
                    {"name": "omar-ea-2", "source": "local-install"},
                    {"name": "user-plugin", "source": "local-install"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        remove_all_omar_antigravity_mcp_configs().unwrap();

        assert!(!plugins_dir.join("omar-ea-1").exists());
        assert!(!plugins_dir.join("omar-ea-2").exists());
        assert!(plugins_dir.join("user-plugin").exists());
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        let imports = manifest["imports"].as_array().unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0]["name"], "user-plugin");
    }

    #[test]
    fn installing_the_antigravity_hook_claims_one_name_and_leaves_the_rest() {
        let _guard = crate::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let previous = std::env::var("HOME").ok();
        std::env::set_var("HOME", home.path());
        let path = home
            .path()
            .join(".gemini")
            .join("config")
            .join("hooks.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"safety-gate":{"enabled":false,"PreToolUse":[{"matcher":"run_command",
                "hooks":[{"command":"./scripts/safety-check.sh"}]}]}}"#,
        )
        .unwrap();

        assert!(install_antigravity_hook());
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        // Their named hook is untouched — the CLI merges names, so ours sits
        // alongside rather than replacing the file.
        assert_eq!(config["safety-gate"]["enabled"], false);
        assert_eq!(
            config["safety-gate"]["PreToolUse"][0]["matcher"],
            "run_command"
        );

        let ours = &config["omar"]["PreInvocation"][0];
        assert_eq!(ours["type"], "command");
        assert!(ours["command"].as_str().unwrap().contains("hook-drain"));
        assert!(ours["command"].as_str().unwrap().contains("--format agy"));

        match previous {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}
