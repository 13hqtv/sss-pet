//! Task 14: TOML-backed `Config` (portable + `%APPDATA%` fallback).
//!
//! Replaces Task 4's in-memory `Mutex<HashMap<...>>` placeholder with
//! a real `config.toml` next to the exe (portable), falling back to
//! `%APPDATA%\swd-pet\config.toml` when the exe dir is read-only.
//! Path resolution is shared with [`crate::db`] — both files always
//! live in the same dir.
//!
//! The five Task 4 Tauri commands ([`save_pet_pos`], [`save_panel_state`],
//! [`load_window_state`], [`load_pet_pos`], [`load_panel_state`]) AND
//! the four Rust-side helpers ([`save_pet_pos_state`] etc.) keep their
//! signatures — they now persist via `Config::load` / `save` instead of
//! the in-memory HashMap. Callers in `window.rs` and `lib.rs` are
//! unchanged.
//!
//! Design ref: `docs/superpowers/specs/2026-10-21-desktop-pet-design.md`
//! ("便携回退" + "配置"): TOML/JSON next to the exe for theme /
//! follow-activity interval / provider creds / focus window / persisted
//! window rect.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::db;

// ===========================================================================
// Config struct + sub-structs
// ===========================================================================

/// Top-level app config. Serialized as `config.toml` next to the exe
/// (or in `%APPDATA%\swd-pet\config.toml` on fallback). Defaults are
/// sensible for a first run with no file — `Config::default()` is what
/// `load()` returns when the file is missing / unreadable.
///
/// `pet_pos` / `panel_rect` are `Option` because they're absent on
/// first run (the user hasn't dragged / resized yet) — Task 4's
/// `load_pet_pos_state` returns `None` then and `lib.rs::run` falls
/// back to the real bottom-right default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// Min seconds between "follow activity" reactions (the pet
    /// glances / shifts when the user types / moves). Default 900 =
    /// 15min, the design spec's floor. Configurable so a future
    /// settings tab can raise it; random variant (spec's "可选随机")
    /// also has 15min as its floor.
    pub follow_activity_min_secs: u64,
    /// Per-provider config (executable path overrides / env / creds).
    /// Empty placeholder — Task 15+ fills it in.
    pub providers: ProvidersCfg,
    /// Theme: `"system"` (default) | `"light"` | `"dark"`.
    pub theme: String,
    /// Focus / do-not-disturb config.
    pub focus: FocusCfg,
    /// Last persisted pet position (physical px). `None` until the
    /// user drags the pet for the first time.
    pub pet_pos: Option<(i32, i32)>,
    /// Last persisted panel rect (x, y, w, h) in physical px. `None`
    /// until the user moves / resizes the panel for the first time.
    pub panel_rect: Option<(i32, i32, u32, u32)>,
    /// Default agent provider (`"claude"` | `"codex"` | `"opencode"`).
    /// Used as the initial value of the SettingsView's default-provider
    /// select + as the provider for a new conversation if the user
    /// hasn't picked one. Defaults to `"claude"`.
    #[serde(default = "default_provider")]
    pub default_provider: String,
    /// Working directory for the agent CLI's cwd (Task 16 carry:
    /// `chat_send` reads this instead of `std::env::current_dir()`).
    /// `None` → fall back to the process's cwd (the previous behavior).
    /// A path field in SettingsView saves here so the user can pin
    /// the project dir the agent runs in (e.g. `C:\code\my-project`).
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Roster of responsible persons for the 代办 (todos) tab. The user
    /// configures this as a comma-separated string in SettingsView; the
    /// TodosView's assignee dropdown reads from this list. Empty by
    /// default (first run). `#[serde(default)]` keeps old config files
    /// loading (the field is absent in pre-v2 configs).
    #[serde(default)]
    pub responsible_persons: Vec<String>,
    /// Roster of KPI types for the KPI tab. Configured as a
    /// comma-separated string in SettingsView (e.g.
    /// "需求开发,Bug修复,技术分享"). The KpisView's type dropdown and
    /// progress-bar section read from this list. Empty by default
    /// (first run). `#[serde(default)]` keeps old config files loading.
    #[serde(default)]
    pub kpi_types: Vec<String>,
}

/// Default for `Config::default_provider`. Kept as a function so
/// `#[serde(default = "...")]` can reference it (serde's default
/// attribute takes a path, not a closure — and the field is a String,
/// not a &str).
fn default_provider() -> String {
    "claude".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            follow_activity_min_secs: 900,
            providers: ProvidersCfg::default(),
            theme: "system".into(),
            focus: FocusCfg::default(),
            pet_pos: None,
            panel_rect: None,
            default_provider: default_provider(),
            working_dir: None,
            responsible_persons: Vec::new(),
            kpi_types: Vec::new(),
        }
    }
}

/// Per-provider config. Task 5: each agent can have a **default model**
/// (probed + picked in `SettingsView`; pre-filled in the new-conversation
/// dialog). The TOML has one `[providers.<agent>]` section per agent
/// (claude / codex / opencode), each carrying an optional `default_model`.
///
/// `#[serde(default)]` on each field so an OLD `config.toml` written
/// before Task 5 (which had an empty `[providers]` table, or no table at
/// all) still loads — the missing sections deserialize to
/// `ProviderCfg::default()` (`default_model: None`), exactly the
/// "user hasn't probed yet" state. This keeps the schema migration
/// back-compat: no rewrite of old config files needed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProvidersCfg {
    /// `[providers.claude]` — Claude's default model. `None` until the
    /// user probes + picks one in SettingsView; `None` = "use the
    /// agent's own default model" (passed as `model: None` to
    /// `new_conversation`).
    #[serde(default)]
    pub claude: ProviderCfg,
    /// `[providers.codex]` — Codex's default model. Same semantics as
    /// `claude`.
    #[serde(default)]
    pub codex: ProviderCfg,
    /// `[providers.opencode]` — OpenCode's default model. Same
    /// semantics as `claude`. Note: opencode's model IDs are
    /// `provider/model` (e.g. `anthropic/claude-…`).
    #[serde(default)]
    pub opencode: ProviderCfg,
}

/// Per-agent config. Task 5: `default_model` is the only field so far
/// (the design's per-agent default-model feature). Future fields
/// (executable_path / env overrides per agent, mirroring multica's
/// `MULTICA_<X>_PATH`) extend this struct without a breaking schema
/// change — `#[serde(default)]` on the parent's field + this struct's
/// `Default` impl keep old TOML loading.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProviderCfg {
    /// The default model for this agent, probed via `list_models(agent)`
    /// + picked by the user in SettingsView. Pre-filled into the
    /// new-conversation dialog's model field. `None` = the agent's own
    /// default model (no `--model` flag passed).
    #[serde(default)]
    pub default_model: Option<String>,
}

impl ProvidersCfg {
    /// Read the default model for `provider` (one of `claude` / `codex`
    /// / `opencode`). Unknown providers return `None`. Used by the
    /// frontend's new-conv dialog to pre-fill the model from the
    /// per-agent default. Also exercised by `providers_default_model
    /// _round_trip` (the test pins the round-trip); the production
    /// path writes the whole `ProvidersCfg` struct via `settings_set`,
    /// so this helper + `set_default_model` are convenience APIs for
    /// future server-side code that wants to touch one agent's slot
    /// without load-modify-saving the whole config.
    #[allow(dead_code)]
    pub fn default_model_for(&self, provider: &str) -> Option<&str> {
        match provider {
            "claude" => self.claude.default_model.as_deref(),
            "codex" => self.codex.default_model.as_deref(),
            "opencode" => self.opencode.default_model.as_deref(),
            _ => None,
        }
    }

    /// Set the default model for `provider` (one of `claude` / `codex`
    /// / `opencode`). Unknown providers are a no-op (the design only
    /// covers the three whitelisted agents). Used by `settings_set` to
    /// persist the per-agent default model picked in SettingsView +
    /// by the round-trip test.
    #[allow(dead_code)]
    pub fn set_default_model(&mut self, provider: &str, model: Option<String>) {
        let slot = match provider {
            "claude" => &mut self.claude,
            "codex" => &mut self.codex,
            "opencode" => &mut self.opencode,
            _ => return,
        };
        slot.default_model = model;
    }
}

/// Focus / do-not-disturb config. `enabled` is the manual toggle
/// (the spec's "手动开关"); `window` is an optional `[start, end]` time
/// range (e.g. `("22:00", "07:00")`) for the spec's "可选时段".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FocusCfg {
    pub enabled: bool,
    pub window: Option<(String, String)>,
}

impl Default for FocusCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            window: None,
        }
    }
}

// ===========================================================================
// load / save (portable + fallback path)
// ===========================================================================

/// Resolved path to `config.toml` — same dir as `swd-pet.db` (shared
/// with [`crate::db::resolve_data_dir`]). `None` if neither exe dir
/// nor appdata is writable.
fn resolve_config_path() -> Option<PathBuf> {
    db::resolve_data_dir().map(|d| d.join("config.toml"))
}

/// Load the config. If the file is missing / unreadable / malformed,
/// returns [`Config::default`] (a first run is indistinguishable from
/// a corrupted file at this layer — Task 15's settings tab can surface
/// parse errors later if needed; for now, defaults are safe).
pub fn load() -> Config {
    let path = match resolve_config_path() {
        Some(p) => p,
        None => return Config::default(),
    };
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

/// Save the config to the resolved path (exe dir, or appdata
/// fallback). Creates the dir if missing. Returns an error if the path
/// can't be resolved OR the write fails — callers (the Tauri
/// commands) surface it as a string.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let path = resolve_config_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no writable data dir for config.toml",
        )
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let s = toml::to_string_pretty(cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, s)
}

// ===========================================================================
// Task 4's WindowState — kept for `load_window_state`'s signature.
// ===========================================================================

/// The persisted state for one window. Kept (not replaced by the new
/// `Config.pet_pos` / `panel_rect` tuples) so [`load_window_state`]'s
/// signature stays stable for Task 4's frontend callers — the command
/// converts from the TOML tuples to this struct on the way out.
///
/// For the `"pet"` window only `x`/`y` are meaningful (the pet is a
/// fixed 120×120 declared in `tauri.conf.json`, so its size isn't
/// persisted — the `w`/`h` fields stay `None`). For the `"panel"`
/// window all four fields are populated because the panel is
/// user-resizable.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub w: Option<u32>,
    pub h: Option<u32>,
}

impl WindowState {
    pub fn pet(x: i32, y: i32) -> Self {
        Self { x, y, w: None, h: None }
    }
    pub fn panel(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w: Some(w), h: Some(h) }
    }
}

// ===========================================================================
// Rust-side helpers (used by window.rs / lib.rs)
// ===========================================================================

/// Persist the pet's position via TOML. Loads the current config,
/// updates `pet_pos`, saves. Best-effort: errors are silently dropped
/// (mirrors Task 4's "in-memory best-effort store" behavior — a
/// failed save shouldn't crash the drag handler).
pub fn save_pet_pos_state(x: i32, y: i32) {
    let mut cfg = load();
    cfg.pet_pos = Some((x, y));
    let _ = save(&cfg);
}

/// Persist the panel's rect via TOML. Same load-modify-save pattern.
pub fn save_panel_state_state(x: i32, y: i32, w: u32, h: u32) {
    let mut cfg = load();
    cfg.panel_rect = Some((x, y, w, h));
    let _ = save(&cfg);
}

/// Load the pet's persisted position, or `None` (first run).
pub fn load_pet_pos_state() -> Option<(i32, i32)> {
    load().pet_pos
}

/// Load the panel's persisted rect, or `None` (first run).
pub fn load_panel_state_state() -> Option<(i32, i32, u32, u32)> {
    load().panel_rect
}

// ===========================================================================
// Tauri commands (frontend-facing, signatures unchanged from Task 4)
// ===========================================================================

/// Persist the pet's position. Called from `Pet.svelte` on `mouseup`
/// after a `data-tauri-drag-region` drag completes.
#[tauri::command]
pub fn save_pet_pos(x: i32, y: i32) -> Result<(), String> {
    save_pet_pos_state(x, y);
    Ok(())
}

/// Persist the panel's rect (position + size). Called from
/// `panel/App.svelte` at the end of a move/resize (debounced via
/// `onMoved` / `onResized`).
#[tauri::command]
pub fn save_panel_state(x: i32, y: i32, w: u32, h: u32) -> Result<(), String> {
    save_panel_state_state(x, y, w, h);
    Ok(())
}

/// Generic load by window label. Returns the full [`WindowState`] (with
/// `w` / `h` as `Option` so the pet's `None` size is preserved) or
/// `None` if no state is stored. Reads from TOML on each call —
/// cheap (a small file), and keeps the in-memory and on-disk views
/// consistent (no stale cache to invalidate).
#[tauri::command]
pub fn load_window_state(label: String) -> Result<Option<WindowState>, String> {
    let cfg = load();
    let state = match label.as_str() {
        "pet" => cfg.pet_pos.map(|(x, y)| WindowState::pet(x, y)),
        "panel" => cfg
            .panel_rect
            .map(|(x, y, w, h)| WindowState::panel(x, y, w, h)),
        _ => None,
    };
    Ok(state)
}

/// Load just the pet's persisted position (or `None`) as a `(x, y)`
/// tuple. Convenience command for the frontend —
/// `load_window_state("pet")` returns the same data with the `x`/`y`
/// fields and `None` size.
#[tauri::command]
pub fn load_pet_pos() -> Result<Option<(i32, i32)>, String> {
    Ok(load_pet_pos_state())
}

/// Load the panel's persisted rect (or `None`) as a `(x, y, w, h)`
/// tuple so the frontend can pass it straight to `setPosition` /
/// `setSize`.
#[tauri::command]
pub fn load_panel_state() -> Result<Option<(i32, i32, u32, u32)>, String> {
    Ok(load_panel_state_state())
}

// ===========================================================================
// Task 16: settings_get / settings_set + cwd carry
// ===========================================================================

/// The shape SettingsView reads/writes via `settings_get` /
/// `settings_set`. A flattened projection of [`Config`] containing only
/// the user-facing fields (NOT `pet_pos` / `panel_rect` — those are
/// owned by the drag/resize paths in `window.rs` / `App.svelte`).
/// Persisting via `settings_set` load-modifies-saves so it doesn't
/// clobber the window-state fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// Min seconds between follow-activity reactions (≥900 = 15min).
    pub follow_activity_min_secs: u64,
    /// Theme: `"system"` | `"light"` | `"dark"`.
    pub theme: String,
    /// Focus / 勿扰 config (enabled + optional time window).
    pub focus: FocusCfg,
    /// Default agent provider (`"claude"` | `"codex"` | `"opencode"`).
    pub default_provider: String,
    /// Working directory for the agent CLI's cwd. `None` → fall back
    /// to the process's cwd. Used as the **default pre-fill** for the
    /// new-conversation dialog's cwd field (Task 5); once a
    /// conversation is created, its own `cwd` (set at creation,
    /// immutable) is the source of truth.
    pub working_dir: Option<String>,
    /// Per-agent config (Task 5: each agent's `default_model`, probed
    /// + picked in SettingsView, pre-filled in the new-conv dialog).
    /// `ProvidersCfg::default()` (all `default_model: None`) is the
    /// first-run / unprobed state.
    #[serde(default)]
    pub providers: ProvidersCfg,
    /// Roster of responsible persons for the 代办 tab. Configured as
    /// comma-separated text in SettingsView; the TodosView's assignee
    /// dropdown reads from this list. `#[serde(default)]` keeps old
    /// configs loading (empty list when the field is absent).
    #[serde(default)]
    pub responsible_persons: Vec<String>,
    /// Roster of KPI types for the KPI tab (comma-separated in
    /// SettingsView, same pattern as `responsible_persons`).
    #[serde(default)]
    pub kpi_types: Vec<String>,
}

impl From<&Config> for Settings {
    fn from(c: &Config) -> Self {
        Self {
            follow_activity_min_secs: c.follow_activity_min_secs,
            theme: c.theme.clone(),
            focus: c.focus.clone(),
            default_provider: c.default_provider.clone(),
            working_dir: c.working_dir.clone(),
            providers: c.providers.clone(),
            responsible_persons: c.responsible_persons.clone(),
            kpi_types: c.kpi_types.clone(),
        }
    }
}

/// Load the user-facing settings (the SettingsView reads this on
/// mount). Reads the full config + projects to [`Settings`].
#[tauri::command]
pub fn settings_get() -> Result<Settings, String> {
    Ok(Settings::from(&load()))
}

/// Persist the user-facing settings (the SettingsView's 保存 button
/// calls this). Load-modify-saves so the non-user-facing fields
/// (`pet_pos` / `panel_rect`) are preserved — the panel's drag/resize
/// path is the only writer for those, and a settings save shouldn't
/// clobber them.
///
/// Side effects beyond the write itself:
/// - The caller (`lib.rs::settings_set` wrapper OR a future `lib.rs`
///   setup hook) re-applies `Signal::FocusMode(focus.enabled)` to the
///   brain so toggling 勿扰 takes effect immediately (without waiting
///   for the brain's 1s tick to re-derive from idle, which it can't
///   — focus is a manual toggle, not derived from idle).
#[tauri::command]
pub fn settings_set(settings: Settings) -> Result<(), String> {
    let mut cfg = load();
    cfg.follow_activity_min_secs = settings.follow_activity_min_secs;
    cfg.theme = settings.theme;
    cfg.focus = settings.focus;
    cfg.default_provider = settings.default_provider;
    cfg.working_dir = settings.working_dir;
    // Task 5: persist the per-agent default models picked in
    // SettingsView (probed via `list_models(agent)` + saved here so
    // the new-conv dialog pre-fills the model from this default).
    cfg.providers = settings.providers;
    cfg.responsible_persons = settings.responsible_persons;
    cfg.kpi_types = settings.kpi_types;
    save(&cfg).map_err(|e| format!("settings_set save failed: {e}"))
}

/// Resolve the agent CLI's working directory from config. Returns the
/// configured `working_dir` if set + non-empty; otherwise falls back
/// to the process's current dir (the pre-Task-16 behavior). Called by
/// `chat::chat_send` so the agent runs in the project dir the user
/// pinned in SettingsView, not the pet's process cwd (which is the
/// exe dir — usually NOT where the user's project lives).
pub fn resolve_agent_cwd() -> std::path::PathBuf {
    if let Some(d) = load().working_dir {
        let trimmed = d.trim();
        if !trimmed.is_empty() {
            return std::path::PathBuf::from(trimmed);
        }
    }
    // Fall back to the process's cwd (preserves the pre-Task-16
    // behavior when the user hasn't pinned a working dir).
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Open a writable temp data dir (overrides `db::resolve_data_dir`
    /// via db's thread-local test override). Returns the tempdir
    /// (kept alive for the test's duration so the dir isn't cleaned up
    /// mid-test) + the guard that clears the override on drop.
    fn fresh_data_dir() -> (tempfile::TempDir, db::TestOverrideGuard) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let g = db::set_test_overrides(Some(tmp.path().to_path_buf()), None, None);
        (tmp, g)
    }

    // -- defaults --

    #[test]
    fn defaults_sensible() {
        let cfg = Config::default();
        assert_eq!(cfg.follow_activity_min_secs, 900, "15min default");
        assert_eq!(cfg.theme, "system");
        assert!(!cfg.focus.enabled);
        assert!(cfg.focus.window.is_none());
        assert!(cfg.pet_pos.is_none());
        assert!(cfg.panel_rect.is_none());
        assert!(cfg.responsible_persons.is_empty(), "empty roster by default");
    }

    // -- round-trip --

    #[test]
    fn config_round_trip() {
        let (_tmp, _g) = fresh_data_dir();
        let cfg = Config {
            follow_activity_min_secs: 1800,
            providers: ProvidersCfg::default(),
            theme: "dark".into(),
            focus: FocusCfg {
                enabled: true,
                window: Some(("22:00".into(), "07:00".into())),
            },
            pet_pos: Some((100, 200)),
            panel_rect: Some((50, 60, 380, 560)),
            default_provider: "codex".into(),
            working_dir: Some("C:\\code\\proj".into()),
            responsible_persons: vec!["alice".into(), "bob".into()],
            kpi_types: vec!["需求开发".into(), "Bug修复".into()],
        };
        save(&cfg).expect("save");
        let loaded = load();
        assert_eq!(loaded, cfg, "Config must round-trip through TOML exactly");
    }

    /// Round-trip with `None` pet_pos / panel_rect (first-run state).
    #[test]
    fn config_round_trip_with_none_fields() {
        let (_tmp, _g) = fresh_data_dir();
        let cfg = Config {
            follow_activity_min_secs: 900,
            providers: ProvidersCfg::default(),
            theme: "system".into(),
            focus: FocusCfg::default(),
            pet_pos: None,
            panel_rect: None,
            default_provider: "claude".into(),
            working_dir: None,
            responsible_persons: Vec::new(),
            kpi_types: Vec::new(),
        };
        save(&cfg).expect("save");
        assert_eq!(load(), cfg);
    }

    /// Task 5: per-agent `default_model` round-trips through TOML +
    /// the `default_model_for` / `set_default_model` helpers work.
    /// This pins the new `ProvidersCfg` schema (three per-agent
    /// sections, each with an optional `default_model`) AND the
    /// back-compat story (an old config with no `[providers.<agent>]`
    /// section loads as `default_model: None`). Also verifies the
    /// `Settings::providers` projection carries the defaults through
    /// `settings_get` / `settings_set`.
    #[test]
    fn providers_default_model_round_trip() {
        let (_tmp, _g) = fresh_data_dir();
        let mut cfg = Config::default();
        // Set a default model for each agent via the helper.
        cfg.providers
            .set_default_model("claude", Some("claude-sonnet-4-5".into()));
        cfg.providers.set_default_model("codex", Some("gpt-5".into()));
        // opencode: leave None (the agent's own default).
        cfg.providers.set_default_model("opencode", None);
        save(&cfg).expect("save");
        let loaded = load();
        assert_eq!(loaded, cfg, "per-agent default_model must round-trip");
        assert_eq!(
            loaded.providers.default_model_for("claude"),
            Some("claude-sonnet-4-5"),
        );
        assert_eq!(
            loaded.providers.default_model_for("codex"),
            Some("gpt-5"),
        );
        assert_eq!(loaded.providers.default_model_for("opencode"), None);
        // Unknown provider → None (the helpers are whitelisted).
        assert_eq!(loaded.providers.default_model_for("unknown"), None);
    }

    /// Task 5: an OLD `config.toml` written before Task 5 (which had an
    /// empty `[providers]` table or no `[providers]` section at all)
    /// still loads — the missing per-agent sections deserialize to
    /// `ProviderCfg::default()` (`default_model: None`). This is the
    /// back-compat guarantee `#[serde(default)]` on each `ProvidersCfg`
    /// field gives. Writes a pre-Task-5 TOML by hand + asserts the
    /// load yields the default ProvidersCfg (no panic, no error).
    #[test]
    fn old_config_without_providers_sections_loads_as_defaults() {
        let (tmp, _g) = fresh_data_dir();
        // A pre-Task-5 config: no [providers.<agent>] sections, just
        // the top-level fields. The empty [providers] table is what
        // v0.1.0 wrote (ProvidersCfg was a unit struct then).
        std::fs::write(
            tmp.path().join("config.toml"),
            "follow_activity_min_secs = 900\ntheme = \"system\"\n\
             default_provider = \"claude\"\n[focus]\nenabled = false\n\
             [providers]\n",
        )
        .expect("write");
        let cfg = load();
        assert_eq!(cfg.providers, ProvidersCfg::default());
        assert_eq!(cfg.providers.claude.default_model, None);
        assert_eq!(cfg.providers.codex.default_model, None);
        assert_eq!(cfg.providers.opencode.default_model, None);
    }

    /// `load()` with no file returns defaults (first run).
    #[test]
    fn load_with_no_file_returns_defaults() {
        let (_tmp, _g) = fresh_data_dir();
        let cfg = load();
        assert_eq!(cfg, Config::default());
    }

    /// A corrupted TOML file is treated like a missing one — defaults.
    /// (Mirrors Task 4's behavior: never block the app from launching
    /// over a config parse error.)
    #[test]
    fn load_with_malformed_toml_returns_defaults() {
        let (tmp, _g) = fresh_data_dir();
        std::fs::write(tmp.path().join("config.toml"), "this is not = valid toml {{{")
            .expect("write");
        let cfg = load();
        assert_eq!(cfg, Config::default());
    }

    // -- Task 4 command rewiring (TOML-backed now, not in-memory) --

    #[test]
    fn save_pet_pos_state_then_load_pet_pos_state_round_trips() {
        let (_tmp, _g) = fresh_data_dir();
        assert!(load_pet_pos_state().is_none(), "no pos on first run");
        save_pet_pos_state(123, 456);
        assert_eq!(load_pet_pos_state(), Some((123, 456)));
        // Overwrite with a new position.
        save_pet_pos_state(789, 12);
        assert_eq!(load_pet_pos_state(), Some((789, 12)));
    }

    #[test]
    fn save_panel_state_state_then_load_panel_state_state_round_trips() {
        let (_tmp, _g) = fresh_data_dir();
        assert!(load_panel_state_state().is_none());
        save_panel_state_state(10, 20, 380, 560);
        assert_eq!(load_panel_state_state(), Some((10, 20, 380, 560)));
    }

    /// Saving the pet pos must NOT clobber the panel rect (and vice
    /// versa) — both go through `Config::load` / `save`, so the
    /// load-modify-save pattern must preserve the other field.
    #[test]
    fn save_pet_pos_does_not_clobber_panel_rect() {
        let (_tmp, _g) = fresh_data_dir();
        save_panel_state_state(10, 20, 380, 560);
        save_pet_pos_state(100, 200);
        // Both should be present.
        assert_eq!(load_pet_pos_state(), Some((100, 200)));
        assert_eq!(load_panel_state_state(), Some((10, 20, 380, 560)));
    }

    /// `load_window_state("pet")` returns a `WindowState` with `None`
    /// size (the pet's size is fixed by tauri.conf.json, not persisted).
    #[test]
    fn load_window_state_pet_returns_none_size() {
        let (_tmp, _g) = fresh_data_dir();
        save_pet_pos_state(50, 60);
        let ws = load_window_state("pet".into()).expect("cmd ok").expect("present");
        assert_eq!(ws.x, 50);
        assert_eq!(ws.y, 60);
        assert!(ws.w.is_none(), "pet size must be None");
        assert!(ws.h.is_none(), "pet size must be None");
    }

    /// `load_window_state("panel")` returns a `WindowState` with both
    /// position + size populated.
    #[test]
    fn load_window_state_panel_returns_position_and_size() {
        let (_tmp, _g) = fresh_data_dir();
        save_panel_state_state(1, 2, 3, 4);
        let ws = load_window_state("panel".into()).expect("cmd ok").expect("present");
        assert_eq!(ws.x, 1);
        assert_eq!(ws.y, 2);
        assert_eq!(ws.w, Some(3));
        assert_eq!(ws.h, Some(4));
    }

    /// `load_window_state` for an unknown label returns `None` (the
    /// pet / panel are the only two windows the app persists).
    #[test]
    fn load_window_state_unknown_label_returns_none() {
        let (_tmp, _g) = fresh_data_dir();
        assert!(load_window_state("unknown".into()).expect("cmd ok").is_none());
    }

    // -- Review Focus #5: read-only exe dir → %APPDATA% fallback --

    /// `save` targets a READ-ONLY exe dir → must fall back to the
    /// appdata dir AND the write succeeds there (the file lands at
    /// the appdata path, not the read-only exe dir).
    #[test]
    fn save_falls_back_to_appdata_when_exe_dir_readonly() {
        let exe_dir = tempfile::tempdir().expect("exe tmp");
        let appdata_dir = tempfile::tempdir().expect("appdata tmp");
        let mut perms = std::fs::metadata(exe_dir.path()).expect("meta").permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(exe_dir.path(), perms).expect("set_readonly");

        let _g = db::set_test_overrides(
            None,
            Some(exe_dir.path().to_path_buf()),
            Some(appdata_dir.path().to_path_buf()),
        );

        // save() must succeed by falling back to appdata.
        let cfg = Config {
            follow_activity_min_secs: 600,
            theme: "dark".into(),
            ..Config::default()
        };
        save(&cfg).expect("save must fall back to appdata, not error");

        // The file is in the appdata fallback, NOT the read-only exe dir.
        let cfg_in_appdata = appdata_dir.path().join("config.toml");
        let cfg_in_exe = exe_dir.path().join("config.toml");
        assert!(
            cfg_in_appdata.exists(),
            "config.toml should be at appdata fallback: {}",
            cfg_in_appdata.display()
        );
        assert!(
            !cfg_in_exe.exists(),
            "config.toml should NOT be in the read-only exe dir: {}",
            cfg_in_exe.display()
        );

        // load() reads the same fallback path → round-trips.
        let loaded = load();
        assert_eq!(loaded.follow_activity_min_secs, 600);
        assert_eq!(loaded.theme, "dark");

        // Restore permissions so tempfile can clean up.
        let mut perms = std::fs::metadata(exe_dir.path()).expect("meta2").permissions();
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(exe_dir.path(), perms);
    }

    /// When the exe dir IS writable, no fallback — the config.toml
    /// lands in the exe dir (portable case, the design spec's default).
    #[test]
    fn save_uses_exe_dir_when_writable() {
        let exe_dir = tempfile::tempdir().expect("exe tmp");
        let _g = db::set_test_overrides(None, Some(exe_dir.path().to_path_buf()), None);
        save(&Config::default()).expect("save");
        let cfg_in_exe = exe_dir.path().join("config.toml");
        assert!(cfg_in_exe.exists(), "config.toml should be in the exe dir (portable)");
    }
}
