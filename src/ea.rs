//! Multi-EA support — EaId type, EA registry, and resolution utilities.
//!
//! Each EA (Executive Assistant) owns a namespace of tmux sessions, a state
//! directory, and an isolated project board / memory / event scope.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// EA identifier. Simple integer.
pub type EaId = u32;

/// Metadata for a registered EA, persisted in ~/.omar/eas.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EaInfo {
    pub id: EaId,
    pub name: String,
    pub description: Option<String>,
    pub created_at: u64, // Unix timestamp
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardLaunchHandoff {
    pub active_ea: EaId,
    pub default_command: String,
    pub default_workdir: String,
    #[serde(default)]
    pub restart_manager: bool,
}

fn default_ea_info() -> EaInfo {
    EaInfo {
        id: 0,
        name: "Default".to_string(),
        description: Some("Primary executive assistant".to_string()),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs(),
    }
}

/// The tmux session prefix for an EA's worker agents.
/// EA 0: "omar-agent-0-"
/// EA 1: "omar-agent-1-"
///
/// IMPORTANT: base_prefix must end with '-' (e.g., "omar-agent-").
/// Do NOT trim it — the trailing '-' separates the base from the ea_id.
pub fn ea_prefix(ea_id: EaId, base_prefix: &str) -> String {
    format!("{}{}-", base_prefix, ea_id)
}

/// The tmux session name for an EA's manager (the EA itself).
/// EA 0: "omar-agent-ea-0"
/// EA 1: "omar-agent-ea-1"
pub fn ea_manager_session(ea_id: EaId, base_prefix: &str) -> String {
    format!("{}ea-{}", base_prefix, ea_id)
}

/// Directory for an EA's state files.
/// EA 0: ~/.omar/ea/0/
/// EA 1: ~/.omar/ea/1/
pub fn ea_state_dir(ea_id: EaId, base_dir: &Path) -> PathBuf {
    base_dir.join("ea").join(ea_id.to_string())
}

fn active_ea_path(base_dir: &Path) -> PathBuf {
    base_dir.join("active_ea")
}

fn dashboard_handoff_path(base_dir: &Path) -> PathBuf {
    base_dir.join("dashboard_handoff.json")
}

/// Load all registered EAs from ~/.omar/eas.json.
pub fn load_registry(base_dir: &Path) -> Vec<EaInfo> {
    let path = base_dir.join("eas.json");
    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(eas) => eas,
            Err(e) => {
                eprintln!(
                    "WARNING: eas.json parse error ({}); treating registry as empty. Check {:?} for corruption.",
                    e, path
                );
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Load the last active EA id persisted on disk.
pub fn load_active_ea(base_dir: &Path) -> Option<EaId> {
    let path = active_ea_path(base_dir);
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<EaId>().ok())
}

/// Persist the active EA id on disk for CLI/dashboard/API defaults.
pub fn save_active_ea(base_dir: &Path, ea_id: EaId) -> anyhow::Result<()> {
    let path = active_ea_path(base_dir);
    fs::create_dir_all(base_dir)?;
    fs::write(&path, ea_id.to_string())?;
    Ok(())
}

pub fn save_dashboard_launch_handoff(
    base_dir: &Path,
    handoff: &DashboardLaunchHandoff,
) -> anyhow::Result<()> {
    let path = dashboard_handoff_path(base_dir);
    fs::create_dir_all(base_dir)?;
    fs::write(&path, serde_json::to_vec(handoff)?)?;
    Ok(())
}

pub fn take_dashboard_launch_handoff(base_dir: &Path) -> Option<DashboardLaunchHandoff> {
    let path = dashboard_handoff_path(base_dir);
    let content = fs::read_to_string(&path).ok()?;
    let _ = fs::remove_file(&path);
    serde_json::from_str(&content).ok()
}

/// Resolve the active EA, falling back to the lowest registered EA when needed.
pub fn resolve_active_ea(base_dir: &Path, eas: &[EaInfo]) -> EaId {
    let fallback = eas.iter().map(|ea| ea.id).min().unwrap_or(0);
    let active = load_active_ea(base_dir)
        .filter(|id| eas.iter().any(|ea| ea.id == *id))
        .unwrap_or(fallback);
    let _ = save_active_ea(base_dir, active);
    active
}

/// Resolve an EA selector from either an explicit `id`/`name` or the persisted active EA.
pub fn resolve_ea_selector(base_dir: &Path, selector: Option<&str>) -> anyhow::Result<EaInfo> {
    let eas = ensure_default_ea(base_dir)?;
    let ea = match selector {
        Some(raw) => {
            if let Ok(id) = raw.parse::<EaId>() {
                eas.iter().find(|ea| ea.id == id).cloned().or_else(|| {
                    if id == 0 && eas.len() == 1 {
                        eas.first().cloned()
                    } else {
                        None
                    }
                })
            } else {
                eas.iter().find(|ea| ea.name == raw).cloned()
            }
        }
        None => {
            let active = resolve_active_ea(base_dir, &eas);
            eas.iter().find(|ea| ea.id == active).cloned()
        }
    };

    ea.ok_or_else(|| anyhow::anyhow!("EA '{}' not found", selector.unwrap_or("<active>")))
}

/// Like `resolve_ea_selector`, but creates a new EA when the selector is a
/// non-numeric, valid name that does not match any registered EA. Numeric
/// selectors that don't match still fail because EA IDs are server-assigned
/// (monotonic and never reused).
///
/// Returns `(EaInfo, was_created)`.
pub fn resolve_or_create_ea_selector(
    base_dir: &Path,
    selector: Option<&str>,
) -> anyhow::Result<(EaInfo, bool)> {
    if let Some(raw) = selector {
        if raw.parse::<EaId>().is_err() {
            let eas = ensure_default_ea(base_dir)?;
            if let Some(ea) = eas.iter().find(|ea| ea.name == raw) {
                return Ok((ea.clone(), false));
            }
            let id = register_ea(base_dir, raw, None)?;
            let ea = load_registry(base_dir)
                .into_iter()
                .find(|ea| ea.id == id)
                .ok_or_else(|| anyhow::anyhow!("Created EA {} missing from registry", id))?;
            return Ok((ea, true));
        }
    }
    Ok((resolve_ea_selector(base_dir, selector)?, false))
}

/// A backend launch always allocates a new EA. Its optional name is not a
/// selector; in particular, it must never replace the active EA's manager.
pub fn create_launch_ea(base_dir: &Path, name: Option<&str>) -> anyhow::Result<EaInfo> {
    let _lock = registry_lock(base_dir)?;
    let eas = load_registry(base_dir);
    let next = eas
        .iter()
        .map(|ea| ea.id)
        .max()
        .unwrap_or(0)
        .max(load_next_id_counter(base_dir))
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("EA ID space exhausted"))?;
    let default_name = next.to_string();
    let name = name.unwrap_or(&default_name);
    if eas.iter().any(|ea| ea.name == name) {
        anyhow::bail!("EA name '{name}' already exists; choose a new name for the new EA");
    }
    let id = register_ea_unlocked(base_dir, name, None)?;
    load_registry(base_dir)
        .into_iter()
        .find(|ea| ea.id == id)
        .ok_or_else(|| anyhow::anyhow!("Created EA {id} missing from registry"))
}

/// Ensure at least one default EA exists on disk.
pub fn ensure_default_ea(base_dir: &Path) -> anyhow::Result<Vec<EaInfo>> {
    let _lock = registry_lock(base_dir)?;
    let mut eas = load_registry(base_dir);
    if eas.is_empty() {
        eas.push(default_ea_info());
        save_registry(base_dir, &eas)?;
        fs::create_dir_all(ea_state_dir(0, base_dir).join("status"))?;
    }

    if load_next_id_counter(base_dir) == 0 && eas.iter().any(|ea| ea.id == 0) {
        save_next_id_counter(base_dir, eas.iter().map(|ea| ea.id).max().unwrap_or(0))?;
    }

    Ok(eas)
}

/// Load the high-water mark counter for EA IDs.
/// Returns 0 if the counter file doesn't exist.
pub fn load_next_id_counter(base_dir: &Path) -> EaId {
    let path = base_dir.join("ea_next_id");
    match fs::read_to_string(&path) {
        Ok(s) => s.trim().parse().unwrap_or(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => {
            eprintln!("warn: reading ea_next_id: {}", e);
            0
        }
    }
}

/// Save the high-water mark counter for EA IDs.
fn save_next_id_counter(base_dir: &Path, next_id: EaId) -> anyhow::Result<()> {
    let path = base_dir.join("ea_next_id");
    fs::create_dir_all(base_dir)?;
    fs::write(&path, next_id.to_string())?;
    Ok(())
}

/// Validate an EA name: must be non-empty, at most 64 chars, and contain only
/// characters in [a-zA-Z0-9_-]. Returns an error describing the violation.
pub fn validate_ea_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        anyhow::bail!("EA name must not be empty");
    }
    if name.len() > 64 {
        anyhow::bail!("EA name must not exceed 64 characters (got {})", name.len());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        anyhow::bail!(
            "EA name '{}' contains invalid characters; only [a-zA-Z0-9_-] are allowed",
            name
        );
    }
    Ok(())
}

/// Register a new EA. Returns the assigned ID.
/// IDs are monotonically increasing and never reused, even after deletion.
pub fn register_ea(base_dir: &Path, name: &str, description: Option<&str>) -> anyhow::Result<EaId> {
    let _lock = registry_lock(base_dir)?;
    register_ea_unlocked(base_dir, name, description)
}

fn registry_lock(base_dir: &Path) -> anyhow::Result<fs::File> {
    fs::create_dir_all(base_dir)?;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(base_dir.join("eas.lock"))?;
    file.lock()?;
    Ok(file)
}

fn register_ea_unlocked(
    base_dir: &Path,
    name: &str,
    description: Option<&str>,
) -> anyhow::Result<EaId> {
    validate_ea_name(name)?;
    let mut eas = load_registry(base_dir);
    let max_existing = eas.iter().map(|e| e.id).max().unwrap_or(0);
    let counter = load_next_id_counter(base_dir);
    // Use whichever is higher to ensure monotonicity even after deletions.
    // Fix V8: Use checked_add to prevent u32 overflow wrapping to 0 (which
    // would collide with EA 0 and violate INV5's uniqueness guarantee).
    let next_id = max_existing.max(counter).checked_add(1).ok_or_else(|| {
        anyhow::anyhow!("EA ID space exhausted (u32::MAX reached). Cannot create more EAs.")
    })?;
    let ea = EaInfo {
        id: next_id,
        name: name.to_string(),
        description: description.map(String::from),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(std::time::Duration::ZERO)
            .as_secs(),
    };
    eas.push(ea);
    save_registry(base_dir, &eas)?;
    // Persist the high-water mark so IDs are never reused after deletion
    save_next_id_counter(base_dir, next_id)?;
    // Create state directory
    let state_dir = ea_state_dir(next_id, base_dir);
    fs::create_dir_all(state_dir.join("status"))?;
    Ok(next_id)
}

/// Remove an EA from the registry. Returns an error if it would remove the last EA.
pub fn unregister_ea(base_dir: &Path, ea_id: EaId) -> anyhow::Result<()> {
    let _lock = registry_lock(base_dir)?;
    let mut eas = load_registry(base_dir);
    if eas.len() <= 1 {
        anyhow::bail!("Cannot delete the only EA; at least one EA must remain");
    }
    if !eas.iter().any(|e| e.id == ea_id) {
        anyhow::bail!("EA {} not found in registry", ea_id);
    }
    crate::supervision::remove_ea(base_dir, ea_id)?;
    eas.retain(|e| e.id != ea_id);
    save_registry(base_dir, &eas)
}

fn save_registry(base_dir: &Path, eas: &[EaInfo]) -> anyhow::Result<()> {
    let path = base_dir.join("eas.json");
    fs::create_dir_all(base_dir)?;
    let json = serde_json::to_string_pretty(eas)?;
    // Atomic write: write to temp file, then rename
    let tmp = base_dir.join(format!(".eas.{}.json.tmp", Uuid::new_v4()));
    fs::write(&tmp, &json)?;
    if let Err(err) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_launch_allocates_numbers_and_never_replaces_an_ea() {
        let dir = tempfile::tempdir().unwrap();
        let first = create_launch_ea(dir.path(), None).unwrap();
        save_active_ea(dir.path(), first.id).unwrap();
        let second = create_launch_ea(dir.path(), None).unwrap();
        assert_eq!(first.name, first.id.to_string());
        assert_eq!(second.name, second.id.to_string());
        assert!(second.id > first.id);
        let named = create_launch_ea(dir.path(), Some("Research")).unwrap();
        assert!(named.id > second.id);
        assert!(create_launch_ea(dir.path(), Some("Research")).is_err());
        assert_eq!(load_registry(dir.path()).len(), 3);
        assert_eq!(load_active_ea(dir.path()), Some(first.id));
    }

    #[test]
    fn simultaneous_launches_get_distinct_ids_and_default_names() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let path = dir.path();
                scope.spawn(move || create_launch_ea(path, None).unwrap());
            }
        });
        let eas = load_registry(dir.path());
        assert_eq!(eas.len(), 8);
        for (index, ea) in eas.iter().enumerate() {
            assert_eq!(ea.id, index as EaId + 1);
            assert_eq!(ea.name, ea.id.to_string());
        }
    }

    #[test]
    fn test_ea_prefix() {
        assert_eq!(ea_prefix(0, "omar-agent-"), "omar-agent-0-");
        assert_eq!(ea_prefix(1, "omar-agent-"), "omar-agent-1-");
        assert_eq!(ea_prefix(42, "omar-agent-"), "omar-agent-42-");
    }

    #[test]
    fn test_ensure_default_ea_bootstraps_registry() {
        let dir = tempfile::tempdir().unwrap();

        let eas = ensure_default_ea(dir.path()).unwrap();

        assert_eq!(eas.len(), 1);
        assert_eq!(eas[0].id, 0);
        assert_eq!(eas[0].name, "Default");
        assert!(ea_state_dir(0, dir.path()).join("status").exists());
        assert_eq!(load_next_id_counter(dir.path()), 0);
    }

    #[test]
    fn test_ensure_default_ea_preserves_existing_counter() {
        let dir = tempfile::tempdir().unwrap();

        save_next_id_counter(dir.path(), 7).unwrap();
        let eas = ensure_default_ea(dir.path()).unwrap();

        assert_eq!(eas.len(), 1);
        assert_eq!(eas[0].id, 0);
        assert_eq!(load_next_id_counter(dir.path()), 7);
    }

    #[test]
    fn test_ensure_default_ea_does_not_insert_missing_zero_when_other_eas_exist() {
        let dir = tempfile::tempdir().unwrap();

        let stale = vec![EaInfo {
            id: 3,
            name: "Research".to_string(),
            description: None,
            created_at: 1234567890,
        }];
        save_registry(dir.path(), &stale).unwrap();

        let eas = ensure_default_ea(dir.path()).unwrap();

        assert_eq!(eas.len(), 1);
        assert_eq!(eas[0].id, 3);
        assert_eq!(eas[0].name, "Research");
        assert!(!ea_state_dir(0, dir.path()).join("status").exists());
    }

    #[test]
    fn test_resolve_ea_selector_numeric_zero_falls_back_to_single_ea() {
        let dir = tempfile::tempdir().unwrap();
        save_registry(
            dir.path(),
            &[EaInfo {
                id: 3,
                name: "Research".to_string(),
                description: None,
                created_at: 1234567890,
            }],
        )
        .unwrap();

        let ea = resolve_ea_selector(dir.path(), Some("0")).unwrap();

        assert_eq!(ea.id, 3);
        assert_eq!(ea.name, "Research");
    }

    #[test]
    fn test_ea_manager_session() {
        assert_eq!(ea_manager_session(0, "omar-agent-"), "omar-agent-ea-0");
        assert_eq!(ea_manager_session(1, "omar-agent-"), "omar-agent-ea-1");
    }

    #[test]
    fn test_ea_state_dir() {
        let base = PathBuf::from("/home/user/.omar");
        assert_eq!(
            ea_state_dir(0, &base),
            PathBuf::from("/home/user/.omar/ea/0")
        );
        assert_eq!(
            ea_state_dir(1, &base),
            PathBuf::from("/home/user/.omar/ea/1")
        );
    }

    #[test]
    fn test_load_registry_empty() {
        let dir = tempfile::tempdir().unwrap();
        let eas = load_registry(dir.path());
        assert_eq!(eas.len(), 0);
    }

    #[test]
    fn test_resolve_active_ea_prefers_persisted_value() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();
        let id1 = register_ea(dir.path(), "Research", None).unwrap();
        let eas = load_registry(dir.path());

        save_active_ea(dir.path(), id1).unwrap();

        assert_eq!(resolve_active_ea(dir.path(), &eas), id1);
    }

    #[test]
    fn test_resolve_active_ea_falls_back_when_persisted_value_missing() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();
        let _ = register_ea(dir.path(), "Research", None).unwrap();
        let eas = load_registry(dir.path());

        save_active_ea(dir.path(), 99).unwrap();

        assert_eq!(resolve_active_ea(dir.path(), &eas), 0);
        assert_eq!(load_active_ea(dir.path()), Some(0));
    }

    #[test]
    fn dashboard_launch_handoff_round_trips_once() {
        let dir = tempfile::tempdir().unwrap();
        let handoff = DashboardLaunchHandoff {
            active_ea: 4,
            default_command: crate::config::resolve_backend("claude").unwrap(),
            default_workdir: "/tmp/omar".to_string(),
            restart_manager: true,
        };

        save_dashboard_launch_handoff(dir.path(), &handoff).unwrap();

        let loaded = take_dashboard_launch_handoff(dir.path()).unwrap();
        assert_eq!(loaded.active_ea, handoff.active_ea);
        assert_eq!(loaded.default_command, handoff.default_command);
        assert_eq!(loaded.default_workdir, handoff.default_workdir);
        assert_eq!(loaded.restart_manager, handoff.restart_manager);
        assert!(take_dashboard_launch_handoff(dir.path()).is_none());
    }

    #[test]
    fn test_resolve_ea_selector_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();
        let id = register_ea(dir.path(), "Research", None).unwrap();

        let ea = resolve_ea_selector(dir.path(), Some("Research")).unwrap();

        assert_eq!(ea.id, id);
        assert_eq!(ea.name, "Research");
    }

    #[test]
    fn test_resolve_or_create_ea_selector_creates_on_name_miss() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();

        let (ea, created) = resolve_or_create_ea_selector(dir.path(), Some("Research")).unwrap();

        assert!(created);
        assert_eq!(ea.name, "Research");
        assert!(load_registry(dir.path())
            .iter()
            .any(|e| e.id == ea.id && e.name == "Research"));
    }

    #[test]
    fn test_resolve_or_create_ea_selector_returns_existing_on_name_hit() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();
        let id = register_ea(dir.path(), "Research", None).unwrap();

        let (ea, created) = resolve_or_create_ea_selector(dir.path(), Some("Research")).unwrap();

        assert!(!created);
        assert_eq!(ea.id, id);
    }

    #[test]
    fn test_resolve_or_create_ea_selector_numeric_miss_errors() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();
        // Register a second EA so the "id=0 + single-EA" fallback in
        // resolve_ea_selector cannot mask the miss.
        let _ = register_ea(dir.path(), "Other", None).unwrap();

        let result = resolve_or_create_ea_selector(dir.path(), Some("99"));
        assert!(result.is_err(), "numeric miss must not auto-create");
    }

    #[test]
    fn test_resolve_or_create_ea_selector_rejects_invalid_name() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();

        let result = resolve_or_create_ea_selector(dir.path(), Some("bad name!"));
        assert!(
            result.is_err(),
            "invalid name must error rather than auto-create"
        );
    }

    #[test]
    fn test_resolve_or_create_ea_selector_no_selector_returns_active() {
        let dir = tempfile::tempdir().unwrap();
        let _ = ensure_default_ea(dir.path()).unwrap();

        let (ea, created) = resolve_or_create_ea_selector(dir.path(), None).unwrap();

        assert!(!created);
        assert_eq!(ea.id, 0);
    }

    #[test]
    fn test_register_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let id = register_ea(dir.path(), "Research", Some("R&D")).unwrap();
        assert_eq!(id, 1);

        let eas = load_registry(dir.path());
        assert_eq!(eas.len(), 1);
        assert!(eas.iter().any(|ea| ea.id == 1 && ea.name == "Research"));
    }

    #[test]
    fn test_unregister() {
        let dir = tempfile::tempdir().unwrap();
        register_ea(dir.path(), "Research", None).unwrap(); // id=1
        register_ea(dir.path(), "Extra", None).unwrap(); // id=2 — needed so 1 can be deleted
        unregister_ea(dir.path(), 1).unwrap();
        let eas = load_registry(dir.path());
        assert_eq!(eas.len(), 1);
        assert!(eas.iter().all(|e| e.id != 1));
    }

    #[test]
    fn test_unregister_any_ea_when_others_exist() {
        // Any EA can be deleted as long as at least one remains.
        let dir = tempfile::tempdir().unwrap();
        let id1 = register_ea(dir.path(), "Alpha", None).unwrap();
        let id2 = register_ea(dir.path(), "Beta", None).unwrap();
        // Delete the first one — should succeed
        assert!(unregister_ea(dir.path(), id1).is_ok());
        // Now only id2 remains — deleting it must fail
        let result = unregister_ea(dir.path(), id2);
        assert!(result.is_err(), "should reject deleting the last EA");
        assert!(result.unwrap_err().to_string().contains("at least one"));
    }

    #[test]
    fn test_unregister_last_ea_is_rejected() {
        // Deleting the last EA must fail.
        let dir = tempfile::tempdir().unwrap();
        let id1 = register_ea(dir.path(), "Alpha", None).unwrap();
        let result = unregister_ea(dir.path(), id1);
        assert!(result.is_err(), "should reject deleting the last EA");
        assert!(result.unwrap_err().to_string().contains("at least one"));
    }

    #[test]
    fn test_unregister_unknown_ea_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        register_ea(dir.path(), "Alpha", None).unwrap();
        register_ea(dir.path(), "Beta", None).unwrap();

        let result = unregister_ea(dir.path(), 99);

        assert!(result.is_err(), "should reject unknown EA id");
        assert!(result.unwrap_err().to_string().contains("not found"));
        assert_eq!(load_registry(dir.path()).len(), 2);
    }

    #[test]
    fn test_manager_not_in_worker_prefix() {
        // Manager session "omar-agent-ea-0" should NOT start with worker prefix "omar-agent-0-"
        let manager = ea_manager_session(0, "omar-agent-");
        let prefix = ea_prefix(0, "omar-agent-");
        assert!(!manager.starts_with(&prefix));
    }

    #[test]
    fn test_ids_monotonic_after_deletion() {
        // IDs should never be reused, even when the highest-ID EA is deleted.
        let dir = tempfile::tempdir().unwrap();

        // Create EA 1 and EA 2
        let id1 = register_ea(dir.path(), "Alpha", None).unwrap();
        assert_eq!(id1, 1);
        let id2 = register_ea(dir.path(), "Beta", None).unwrap();
        assert_eq!(id2, 2);

        // Delete EA 2 (the highest); EA 1 still remains
        unregister_ea(dir.path(), 2).unwrap();

        // Create a new EA — should get ID 3, NOT 2
        let id3 = register_ea(dir.path(), "Gamma", None).unwrap();
        assert_eq!(id3, 3, "ID should be 3 (monotonic), not 2 (reused)");

        // Delete EA 1 (EA 3 still remains as the single survivor)
        unregister_ea(dir.path(), 1).unwrap();

        // Create another — should get ID 4, NOT 1 or 2
        let id4 = register_ea(dir.path(), "Delta", None).unwrap();
        assert_eq!(id4, 4, "ID should be 4 (monotonic), not a reused ID");
    }

    #[test]
    fn test_ids_monotonic_without_counter_file() {
        // If the counter file is missing (e.g., upgraded from old version),
        // IDs should still work correctly based on max existing ID.
        let dir = tempfile::tempdir().unwrap();

        let id1 = register_ea(dir.path(), "First", None).unwrap();
        assert_eq!(id1, 1);

        // Manually delete the counter file to simulate upgrade scenario
        let counter_path = dir.path().join("ea_next_id");
        if counter_path.exists() {
            fs::remove_file(&counter_path).unwrap();
        }

        // Should still use max(existing) + 1 = 2
        let id2 = register_ea(dir.path(), "Second", None).unwrap();
        assert_eq!(id2, 2);
    }
}
