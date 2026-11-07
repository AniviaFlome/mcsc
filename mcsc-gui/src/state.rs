use std::path::{Path, PathBuf};

use rune::{AppHandle, TextEdit};

use mcsc_core::level::LevelInfo;
use mcsc_core::{Detection, TargetLayout};

// ── Target ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    #[default]
    VanillaModern,
    Vanilla,
    PaperModern,
    LegacySplit,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::VanillaModern => "Vanilla / Fabric 26.1+",
            Target::Vanilla => "Vanilla / Fabric pre-26.1",
            Target::PaperModern => "Paper modern",
            Target::LegacySplit => "Legacy split",
        }
    }
    pub fn to_core(self) -> TargetLayout {
        match self {
            Target::VanillaModern => TargetLayout::VanillaModern,
            Target::Vanilla => TargetLayout::Vanilla,
            Target::PaperModern => TargetLayout::PaperModern,
            Target::LegacySplit => TargetLayout::LegacySplit,
        }
    }
    pub fn tag(self) -> &'static str {
        match self {
            Target::VanillaModern => "vanilla26",
            Target::Vanilla => "vanilla",
            Target::PaperModern => "papermodern",
            Target::LegacySplit => "legacy",
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ── Banner ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerKind {
    Info,
    Working,
    Ok,
    Err,
}

#[derive(Debug, Clone)]
pub struct Banner {
    pub kind: BannerKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    Ok,
    Neutral,
    Err,
}

// ── State ───────────────────────────────────────────────

pub struct App {
    pub dark: bool,
    pub server_root: TextEdit,
    pub level_name: TextEdit,
    pub target: Target,
    pub backup: bool,
    pub dry_run: bool,
    pub overwrite: bool,
    pub in_place: bool,
    pub player_name: TextEdit,
    pub online_uuid: TextEdit,
    pub convert_all: bool,
    pub auto_remap: bool,
    pub many: Option<mcsc_core::ConvertManyReport>,
    pub output_dir: TextEdit,
    pub output_customized: bool,
    pub detection: Option<Detection>,
    pub level_info: Option<LevelInfo>,
    pub level_err: Option<String>,
    pub scan: Option<mcsc_core::ScanReport>,
    pub convert: Option<mcsc_core::ConvertReport>,
    pub convert_err: Option<String>,
    pub busy: Option<String>,
    pub progress: Option<mcsc_core::Progress>,
    pub banner: Option<Banner>,
    pub log: String,
    pub handle: Option<AppHandle<App>>,
}

impl App {
    pub fn new() -> Self {
        Self {
            dark: true,
            server_root: TextEdit::new(""),
            level_name: TextEdit::new("world"),
            target: Target::VanillaModern,
            backup: true,
            dry_run: false,
            overwrite: false,
            in_place: false,
            player_name: TextEdit::new(""),
            online_uuid: TextEdit::new(""),
            convert_all: false,
            auto_remap: true,
            many: None,
            output_dir: TextEdit::new(""),
            output_customized: false,
            detection: None,
            level_info: None,
            level_err: None,
            scan: None,
            convert: None,
            convert_err: None,
            busy: None,
            progress: None,
            banner: None,
            log: String::new(),
            handle: None,
        }
    }

    pub fn new_with_handle(handle: AppHandle<App>) -> Self {
        let mut s = Self::new();
        s.handle = Some(handle);
        s
    }

    pub fn push(&mut self, s: &str) {
        if !self.log.is_empty() {
            self.log.push('\n');
        }
        self.log.push_str(s);
        // Bound memory and layout cost: a convert can emit thousands of
        // per-file lines. Keep the tail; Clear resets fully.
        const MAX_LOG_LINES: usize = 1000;
        let lines = self.log.lines().count();
        if lines > MAX_LOG_LINES {
            let skip: String = self
                .log
                .lines()
                .skip(lines - MAX_LOG_LINES)
                .collect::<Vec<_>>()
                .join("\n");
            self.log = skip;
        }
    }

    pub fn root_path(&self) -> PathBuf {
        PathBuf::from(self.server_root.text.trim())
    }

    /// Resolve what the user picked into a (server_root, level_name) pair
    /// WITHOUT rewriting the input field. Accepts both a server folder
    /// (`<root>/<level>/`) and a world folder itself (`<root>/` containing
    /// level.dat): in the latter case the parent acts as server root and the
    /// folder name as level. Returns the note to show when the picked path
    /// was interpreted as a world folder, or None when taken literally.
    pub fn resolve_dirs(&self) -> (PathBuf, String, Option<String>) {
        let root = self.root_path();
        if root.as_os_str().is_empty() {
            return (root, self.level_name.text.trim().to_string(), None);
        }
        let level = self.effective_level(&root);
        if root.join(&level).is_dir() {
            return (root, level, None);
        }
        let looks_like_world = root.join("level.dat").is_file() || root.join("region").is_dir();
        if looks_like_world {
            if let (Some(parent), Some(name)) = (root.parent(), root.file_name()) {
                let name = name.to_string_lossy().to_string();
                let note = format!(
                    "Picked '{root}' looks like a world folder — using '{parent}' as server folder, level '{name}'.",
                    root = root.display(),
                    parent = parent.display(),
                );
                return (parent.to_path_buf(), name, Some(note));
            }
        }
        (root, level, None)
    }

    pub fn effective_level(&self, root: &Path) -> String {
        if self.level_name.text.trim().is_empty() {
            mcsc_core::detect::read_level_name(root, "world")
        } else {
            self.level_name.text.trim().to_string()
        }
    }

    /// True while a scan/convert task is running.
    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// Output base for copy mode: explicit choice, else the server folder.
    /// Empty means default.
    pub fn output_base(&self) -> Option<PathBuf> {
        let t = self.output_dir.text.trim();
        if t.is_empty() {
            None
        } else {
            Some(PathBuf::from(t))
        }
    }

    /// Live preview of where the converted copy will land.
    pub fn output_preview(&self) -> String {
        if self.in_place {
            return "In-place mode: no copy, the source folder itself changes.".to_string();
        }
        let tag = self.target.tag();
        // Resolve first so picking a world folder previews the same base the
        // convert will actually use (its parent), not the world itself.
        let (root, level, _) = self.resolve_dirs();
        let level = if level.trim().is_empty() {
            "world".to_string()
        } else {
            level
        };
        let base = self
            .output_base()
            .unwrap_or_else(|| mcsc_core::default_output_base(&root))
            .display()
            .to_string();
        format!("Converted copy will be: {base}/{level}-converted-{tag}/")
    }

    /// Live status of the player mapping inputs (or why they won't run).
    pub fn player_hint(&self) -> (String, HintKind) {
        match resolve_remap(self) {
            Ok(Some((from, to))) => (
                format!(
                    "Will remap {}… → {}….",
                    &from[..8.min(from.len())],
                    &to[..8.min(to.len())]
                ),
                HintKind::Ok,
            ),
            Ok(None) => ("Leave both blank to skip.".to_string(), HintKind::Neutral),
            Err(e) => (e, HintKind::Err),
        }
    }

    /// Mirror the default output next to the server folder, unless customized.
    pub fn refresh_output_default(&mut self) {
        if !self.output_customized {
            let base = mcsc_core::default_output_base(&self.root_path());
            self.output_dir.text = base.display().to_string();
        }
    }
}

/// Look up a username's online (v4) UUID via the public playerdb API.
/// Blocking with a short timeout — runs on the worker thread only.
pub fn lookup_online_uuid(username: &str) -> Option<String> {
    if username.is_empty()
        || username.len() > 16
        || !username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let url = format!("https://playerdb.co/api/player/minecraft/{username}");
    let resp = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
        .ok()?;
    let text = resp.into_string().ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    if json.get("success")?.as_bool() != Some(true) {
        return None;
    }
    let raw = json.get("data")?.get("player")?.get("id")?.as_str()?;
    let hex: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    let uuid = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    if mcsc_core::is_uuid(&uuid) {
        Some(uuid)
    } else {
        None
    }
}

/// Resolve the player remap from the two inputs. Ok(None) = skip,
/// Ok(Some) = usable pair, Err = explain what's missing/invalid.
pub fn resolve_remap(app: &App) -> Result<Option<(String, String)>, String> {
    let name = app.player_name.text.trim();
    let online = app.online_uuid.text.trim();
    if name.is_empty() && online.is_empty() {
        return Ok(None);
    }
    if name.is_empty() {
        return Err("Enter the offline username too (needed to find the old player file).".into());
    }
    if online.is_empty() {
        return Err("Enter the online UUID too (your current account id).".into());
    }
    if !mcsc_core::is_uuid(online) {
        return Err(format!(
            "Online UUID doesn't look like a UUID (8-4-4-4-12 hex): '{online}'."
        ));
    }
    let offline = mcsc_core::offline_uuid(name);
    if offline == online {
        return Ok(None);
    }
    Ok(Some((offline, online.to_string())))
}

pub fn do_detect(app: &mut App) {
    // Detect is synchronous and rewrites STATUS; running it mid-task races
    // the worker's completion update. Scan/convert already guard this way.
    if app.is_busy() {
        app.push("Still working — wait for the current task to finish.");
        return;
    }
    let (root, level, note) = app.resolve_dirs();
    if let Some(n) = note {
        app.push(&n);
    }
    detect_into(app, &root, &level);
}

/// Detect layout at an explicit location and refresh STATUS + banner.
/// Used for the source folder and, after converting, for the result.
pub fn detect_into(app: &mut App, root: &Path, level: &str) {
    if root.as_os_str().is_empty() {
        app.push("Set server root first.");
        return;
    }
    let det = mcsc_core::detect_layout(root, level);
    app.push(&format!(
        "Detected: {} @ {}",
        det.layout.label(),
        root.display()
    ));
    let info_path = root.join(level).join("level.dat");
    if info_path.is_file() {
        match mcsc_core::level::read_level_info(&info_path) {
            Ok(info) => {
                app.push(&format!(
                    "level.dat: DataVersion={:?} LevelName={:?}",
                    info.data_version, info.level_name
                ));
                app.level_info = Some(info);
                app.level_err = None;
            }
            Err(e) => {
                app.push(&format!("level.dat parse failed: {e}"));
                app.level_info = None;
                app.level_err = Some(e.to_string());
            }
        }
    } else {
        app.level_info = None;
        app.level_err = Some("no level.dat found".into());
    }
    app.detection = Some(det.clone());
    app.banner = Some(Banner {
        kind: BannerKind::Info,
        text: format!(
            "Detected: {} — ready to scan or convert.",
            det.layout.label()
        ),
    });
    app.convert = None;
    app.convert_err = None;
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(server_root: &str, level_name: &str) -> App {
        let mut app = App::new();
        app.server_root.text = server_root.to_string();
        app.level_name.text = level_name.to_string();
        app
    }

    #[test]
    fn resolve_keeps_server_root() {
        let dir = std::env::temp_dir().join("mcsc-gui-resolve");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("world")).unwrap();
        let app = app_with(&dir.display().to_string(), "world");
        let (root, level, note) = app.resolve_dirs();
        assert_eq!(root, dir);
        assert_eq!(level, "world");
        assert!(note.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_world_dir_to_parent_without_rewriting_field() {
        let dir = std::env::temp_dir().join("mcsc-gui-resolve2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("myworld").join("region")).unwrap();
        std::fs::write(dir.join("myworld").join("level.dat"), b"x").unwrap();
        let before = dir.join("myworld").display().to_string();
        let app = app_with(&before, "world");
        let (root, level, note) = app.resolve_dirs();
        assert_eq!(root, dir);
        assert_eq!(level, "myworld");
        assert!(note.is_some());
        // The input field itself is untouched.
        assert_eq!(app.server_root.text, before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_base_empty_means_default() {
        let mut app = app_with("/srv/mc", "world");
        assert_eq!(app.output_base(), None);
        app.output_dir.text = "   ".to_string();
        assert_eq!(app.output_base(), None);
        app.output_dir.text = "/tmp/out".to_string();
        assert_eq!(
            app.output_base(),
            Some(std::path::PathBuf::from("/tmp/out"))
        );
    }

    #[test]
    fn resolve_remap_needs_both_fields() {
        let mut app = app_with("/srv/mc", "world");
        // Blank pair skips.
        assert_eq!(resolve_remap(&app), Ok(None));
        // Only one side set explains what's missing.
        app.player_name.text = "Anivia31".to_string();
        assert!(resolve_remap(&app).is_err());
        app.player_name.text.clear();
        app.online_uuid.text = "10b8a89c-bb59-4113-a62f-5ac96b915dd9".to_string();
        assert!(resolve_remap(&app).is_err());
        // Bad UUID shape is rejected.
        app.player_name.text = "Anivia31".to_string();
        app.online_uuid.text = "nope".to_string();
        assert!(resolve_remap(&app).is_err());
        // Real pair resolves through the offline-UUID derivation.
        app.online_uuid.text = "10b8a89c-bb59-4113-a62f-5ac96b915dd9".to_string();
        assert_eq!(
            resolve_remap(&app),
            Ok(Some((
                "5ad08d99-26a2-331c-8f77-d476dec25826".to_string(),
                "10b8a89c-bb59-4113-a62f-5ac96b915dd9".to_string()
            )))
        );
    }

    #[test]
    fn detect_waits_when_busy() {
        let mut app = app_with("/srv/mc", "world");
        app.busy = Some("Scanning…".into());
        do_detect(&mut app);
        assert!(
            app.detection.is_none(),
            "detect must not rewrite STATUS mid-task"
        );
        assert!(app.log.contains("Still working"));
    }

    #[test]
    fn output_preview_follows_world_folder_resolution() {
        let dir = std::env::temp_dir().join("mcsc-gui-preview");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("myworld").join("region")).unwrap();
        std::fs::write(dir.join("myworld").join("level.dat"), b"x").unwrap();
        let app = app_with(&dir.join("myworld").display().to_string(), "world");
        let preview = app.output_preview();
        let expect_base = dir.parent().unwrap().display().to_string();
        assert!(
            preview.contains(&format!("{expect_base}/myworld-converted-vanilla26")),
            "preview must use the resolved parent + world name, got: {preview}"
        );
        assert!(
            !preview.contains("myworld/myworld"),
            "preview must not nest the world inside itself, got: {preview}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_keeps_only_the_tail() {
        let mut app = app_with("/srv/mc", "world");
        for i in 0..1500 {
            app.push(&format!("line {i}"));
        }
        assert!(app.log.lines().count() <= 1000);
        assert!(app.log.contains("line 1499"));
    }
}
