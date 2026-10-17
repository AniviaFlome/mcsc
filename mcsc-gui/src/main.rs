use iced::widget::{
    button, checkbox, column, container, row, rule, scrollable, space, text, text_input,
};
use iced::{Color, Element, Length, Subscription, Task};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use mcsc_core::level::LevelInfo;
use mcsc_core::Detection;
use mcsc_core::{
    ConvertManyReport, ConvertOptions, ConvertReport, Progress, ProgressReporter, ScanOptions,
    ScanReport, TargetLayout,
};

/// Handoff from worker threads (spawn_blocking) to the UI subscription.
/// One task runs at a time; each new task replaces the receiver.
static PROGRESS_RX: OnceLock<Mutex<Option<futures::channel::mpsc::UnboundedReceiver<Progress>>>> =
    OnceLock::new();

fn progress_slot() -> &'static Mutex<Option<futures::channel::mpsc::UnboundedReceiver<Progress>>> {
    PROGRESS_RX.get_or_init(|| Mutex::new(None))
}

/// Stream of worker progress updates, active only while a task runs.
fn drain_progress() -> impl futures::Stream<Item = Message> {
    let mut rx = progress_slot().lock().ok().and_then(|mut s| s.take());
    futures::stream::poll_fn(move |cx| {
        use futures::StreamExt;
        use std::task::Poll;
        match rx.as_mut() {
            Some(r) => r.poll_next_unpin(cx).map(|opt| opt.map(Message::Progress)),
            None => Poll::Pending,
        }
    })
}

fn subscription(app: &App) -> Subscription<Message> {
    if app.is_busy() {
        Subscription::run(drain_progress)
    } else {
        Subscription::none()
    }
}

fn app_theme(state: &App) -> iced::Theme {
    if state.dark {
        iced::Theme::CatppuccinMocha
    } else {
        iced::Theme::CatppuccinLatte
    }
}

fn main() -> iced::Result {
    iced::application(|| (App::new(), Task::none()), update, view)
        .title("mcsc — Minecraft Server World Converter")
        .theme(app_theme)
        .subscription(subscription)
        .window_size(iced::Size::new(1152.0, 800.0))
        .run()
}

// ── Target ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Target {
    #[default]
    VanillaModern,
    Vanilla,
    PaperModern,
    LegacySplit,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Target::VanillaModern => "Vanilla / Fabric 26.1+",
            Target::Vanilla => "Vanilla / Fabric pre-26.1",
            Target::PaperModern => "Paper modern",
            Target::LegacySplit => "Legacy split",
        }
    }
    fn hint(self) -> &'static str {
        match self {
            Target::VanillaModern => "Modern unified layout — terrain stays, seed + players fixed",
            Target::Vanilla => "Legacy Anvil layout with DIM folders, seed in level.dat",
            Target::PaperModern => "Paper 26.1+ — adopts vanilla layout on boot",
            Target::LegacySplit => "Spigot / CraftBukkit — old DIM split",
        }
    }
    fn to_core(self) -> TargetLayout {
        match self {
            Target::VanillaModern => TargetLayout::VanillaModern,
            Target::Vanilla => TargetLayout::Vanilla,
            Target::PaperModern => TargetLayout::PaperModern,
            Target::LegacySplit => TargetLayout::LegacySplit,
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

// ── State ───────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Message {
    ServerRootChanged(String),
    LevelNameChanged(String),
    TargetSelected(Target),
    BackupToggled(bool),
    DryRunToggled(bool),
    OverwriteToggled(bool),
    InPlaceToggled(bool),
    PlayerNameChanged(String),
    OnlineUuidChanged(String),
    ConvertAllToggled(bool),
    AutoRemapToggled(bool),
    OutputDirChanged(String),
    BrowseOutput,
    ToggleTheme,
    Browse,
    Detect,
    Scan,
    Progress(Progress),
    ScanFinished(ScanReport),
    Convert,
    ConvertFinished(Result<ConvertReport, String>),
    ConvertManyFinished(ConvertManyReport),
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerKind {
    Info,
    Working,
    Ok,
    Err,
}

#[derive(Debug, Clone)]
struct Banner {
    kind: BannerKind,
    text: String,
}

struct App {
    dark: bool,
    server_root: String,
    level_name: String,
    target: Target,
    backup: bool,
    dry_run: bool,
    overwrite: bool,
    in_place: bool,
    player_name: String,
    online_uuid: String,
    convert_all: bool,
    auto_remap: bool,
    many: Option<ConvertManyReport>,
    output_dir: String,
    output_customized: bool,
    detection: Option<Detection>,
    level_info: Option<LevelInfo>,
    level_err: Option<String>,
    scan: Option<ScanReport>,
    convert: Option<ConvertReport>,
    convert_err: Option<String>,
    busy: Option<String>,
    progress: Option<Progress>,
    banner: Option<Banner>,
    log: String,
}

impl App {
    fn new() -> Self {
        Self {
            dark: true,
            server_root: String::new(),
            level_name: "world".into(),
            target: Target::VanillaModern,
            backup: true,
            dry_run: false,
            overwrite: false,
            in_place: false,
            player_name: String::new(),
            online_uuid: String::new(),
            convert_all: false,
            auto_remap: true,
            many: None,
            output_dir: String::new(),
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
        }
    }
    fn push(&mut self, s: &str) {
        if !self.log.is_empty() {
            self.log.push('\n');
        }
        self.log.push_str(s);
    }
    fn root_path(&self) -> PathBuf {
        PathBuf::from(self.server_root.trim())
    }
    /// Resolve what the user picked into a (server_root, level_name) pair
    /// WITHOUT rewriting the input field. Accepts both a server folder
    /// (`<root>/<level>/`) and a world folder itself (`<root>/` containing
    /// level.dat): in the latter case the parent acts as server root and the
    /// folder name as level. Returns the note to show when the picked path
    /// was interpreted as a world folder, or None when taken literally.
    fn resolve_dirs(&self) -> (PathBuf, String, Option<String>) {
        let root = self.root_path();
        if root.as_os_str().is_empty() {
            return (root, self.level_name.trim().to_string(), None);
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
    fn effective_level(&self, root: &Path) -> String {
        if self.level_name.trim().is_empty() {
            mcsc_core::detect::read_level_name(root, "world")
        } else {
            self.level_name.trim().to_string()
        }
    }
    /// True while a scan/convert task is running. Buttons stay enabled so a
    /// click always gives feedback instead of silently doing nothing.
    fn is_busy(&self) -> bool {
        self.busy.is_some()
    }
    /// Output base for copy mode: explicit choice, else the server folder.
    /// Empty means default.
    fn output_base(&self) -> Option<PathBuf> {
        let t = self.output_dir.trim();
        if t.is_empty() {
            None
        } else {
            Some(PathBuf::from(t))
        }
    }
}

// ── Update ──────────────────────────────────────────────

fn message_name(msg: &Message) -> &'static str {
    match msg {
        Message::ServerRootChanged(_) => "ServerRootChanged",
        Message::LevelNameChanged(_) => "LevelNameChanged",
        Message::TargetSelected(_) => "TargetSelected",
        Message::BackupToggled(_) => "BackupToggled",
        Message::DryRunToggled(_) => "DryRunToggled",
        Message::OverwriteToggled(_) => "OverwriteToggled",
        Message::InPlaceToggled(_) => "InPlaceToggled",
        Message::PlayerNameChanged(_) => "PlayerNameChanged",
        Message::OnlineUuidChanged(_) => "OnlineUuidChanged",
        Message::ConvertAllToggled(_) => "ConvertAllToggled",
        Message::AutoRemapToggled(_) => "AutoRemapToggled",
        Message::OutputDirChanged(_) => "OutputDirChanged",
        Message::BrowseOutput => "BrowseOutput",
        Message::ToggleTheme => "ToggleTheme",
        Message::Browse => "Browse",
        Message::Detect => "Detect",
        Message::Scan => "Scan",
        Message::Progress(_) => "Progress",
        Message::ScanFinished(_) => "ScanFinished",
        Message::Convert => "Convert",
        Message::ConvertFinished(_) => "ConvertFinished",
        Message::ConvertManyFinished(_) => "ConvertManyFinished",
        Message::Clear => "Clear",
    }
}

/// Look up a username's online (v4) UUID via the public playerdb API.
/// Blocking with a short timeout — runs on the worker thread only.
fn lookup_online_uuid(username: &str) -> Option<String> {
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
fn resolve_remap(app: &App) -> Result<Option<(String, String)>, String> {
    let name = app.player_name.trim();
    let online = app.online_uuid.trim();
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

fn do_detect(app: &mut App) {
    let (root, level, note) = app.resolve_dirs();
    if let Some(n) = note {
        app.push(&n);
    }
    detect_into(app, &root, &level);
}

/// Detect layout at an explicit location and refresh STATUS + banner.
/// Used for the source folder and, after converting, for the result.
fn detect_into(app: &mut App, root: &Path, level: &str) {
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

fn update(app: &mut App, msg: Message) -> Task<Message> {
    // Typing is too noisy for the terminal; log actions.
    if !matches!(
        msg,
        Message::ServerRootChanged(_)
            | Message::LevelNameChanged(_)
            | Message::PlayerNameChanged(_)
            | Message::OnlineUuidChanged(_)
            | Message::OutputDirChanged(_)
            | Message::Progress(_)
    ) {
        eprintln!("[mcsc] message: {}", message_name(&msg));
    }
    match msg {
        Message::ServerRootChanged(s) => {
            app.server_root = s.clone();
            app.detection = None;
            if !app.output_customized {
                // Mirror the new default: next to the server folder.
                app.output_dir = mcsc_core::default_output_base(&PathBuf::from(s.trim()))
                    .display()
                    .to_string();
            }
        }
        Message::LevelNameChanged(s) => {
            app.level_name = s;
            app.detection = None;
        }
        Message::TargetSelected(t) => app.target = t,
        Message::BackupToggled(v) => app.backup = v,
        Message::DryRunToggled(v) => app.dry_run = v,
        Message::OverwriteToggled(v) => app.overwrite = v,
        Message::InPlaceToggled(v) => app.in_place = v,
        Message::PlayerNameChanged(s) => app.player_name = s,
        Message::OnlineUuidChanged(s) => app.online_uuid = s,
        Message::ConvertAllToggled(v) => app.convert_all = v,
        Message::AutoRemapToggled(v) => app.auto_remap = v,
        Message::OutputDirChanged(s) => {
            app.output_dir = s;
            app.output_customized = true;
        }
        Message::BrowseOutput => {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                app.output_dir = dir.display().to_string();
                app.output_customized = true;
            }
        }
        Message::ToggleTheme => {
            app.dark = !app.dark;
        }
        Message::Clear => {
            app.scan = None;
            app.convert = None;
            app.convert_err = None;
            app.many = None;
            app.banner = None;
            app.progress = None;
            app.log.clear();
        }
        Message::Browse => {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                // Keep exactly what was picked; resolution happens per
                // operation and is reported, never by rewriting the field.
                app.server_root = dir.display().to_string();
                if !app.output_customized {
                    app.output_dir =
                        mcsc_core::default_output_base(&PathBuf::from(app.server_root.trim()))
                            .display()
                            .to_string();
                }
                app.detection = None;
                // Immediately detect so the user gets instant feedback.
                do_detect(app);
            }
        }
        Message::Detect => {
            do_detect(app);
        }
        Message::Scan => {
            if app.is_busy() {
                app.push("Still working — wait for the current task to finish.");
                return Task::none();
            }
            let (root, level, note) = app.resolve_dirs();
            if root.as_os_str().is_empty() {
                app.push("Pick a server folder first (Browse or type the path), then Scan.");
                return Task::none();
            }
            if let Some(n) = note {
                app.push(&n);
            }
            let (tx, rx) = futures::channel::mpsc::unbounded::<Progress>();
            if let Ok(mut slot) = progress_slot().lock() {
                *slot = Some(rx);
            }
            let reporter = ProgressReporter::new(move |p| {
                let _ = tx.unbounded_send(p);
            });
            app.busy = Some("Scanning region files…".into());
            app.progress = None;
            app.banner = Some(Banner {
                kind: BannerKind::Working,
                text: "Scan started…".into(),
            });
            let opts = ScanOptions {
                server_root: root,
                level_name: level,
                max_files: 0,
                progress: Some(reporter),
            };
            // spawn_blocking: region parsing is CPU/IO heavy and must not
            // stall the async executor (a stalled executor looks like a
            // frozen UI with buttons that never respond).
            return Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || mcsc_core::scan_modded(&opts))
                        .await
                        .unwrap_or_default()
                },
                Message::ScanFinished,
            );
        }
        Message::Progress(p) => {
            app.progress = Some(p);
        }
        Message::ScanFinished(rep) => {
            app.busy = None;
            app.progress = None;
            let summary = format!(
                "Scan complete: {} files, {} chunks, {} with modded IDs.",
                rep.files_scanned, rep.chunks_scanned, rep.chunks_with_modded
            );
            app.push(&summary);
            app.banner = Some(Banner {
                kind: BannerKind::Ok,
                text: summary.clone(),
            });
            app.scan = Some(rep);
        }
        Message::Convert => {
            if app.is_busy() {
                app.push("Still working — wait for the current task to finish.");
                return Task::none();
            }
            let (root, level, note) = app.resolve_dirs();
            if root.as_os_str().is_empty() {
                app.push("Pick a server folder first (Browse or type the path), then Convert.");
                return Task::none();
            }
            if let Some(n) = note {
                app.push(&n);
            }
            let uuid_remap = match resolve_remap(app) {
                Ok(r) => r,
                Err(e) => {
                    app.push(&e);
                    return Task::none();
                }
            };
            let (tx, rx) = futures::channel::mpsc::unbounded::<Progress>();
            if let Ok(mut slot) = progress_slot().lock() {
                *slot = Some(rx);
            }
            let reporter = ProgressReporter::new(move |p| {
                let _ = tx.unbounded_send(p);
            });
            let opts = ConvertOptions {
                server_root: root,
                level_name: level,
                target: app.target.to_core(),
                backup: app.backup,
                dry_run: app.dry_run,
                overwrite: app.overwrite,
                progress: Some(reporter),
                in_place: app.in_place,
                uuid_remap,
                output_dir: app.output_base(),
                auto_remap_all: app.auto_remap,
                uuid_resolver: app
                    .auto_remap
                    .then(|| mcsc_core::UuidResolver::new(lookup_online_uuid)),
            };
            let mode = if opts.dry_run { "dry run" } else { "REAL" };
            // Convert-all: same options for every detected world set.
            let worlds: Option<Vec<String>> = if app.convert_all {
                app.detection
                    .as_ref()
                    .map(|d| d.worlds.iter().map(|w| w.level_name.clone()).collect())
            } else {
                None
            };
            match worlds {
                Some(list) if !list.is_empty() => {
                    app.busy = Some(format!("Converting {} worlds ({mode})…", list.len()));
                    app.progress = None;
                    app.banner = Some(Banner {
                        kind: BannerKind::Working,
                        text: format!("Convert started for {} worlds ({mode})…", list.len()),
                    });
                    return Task::perform(
                        async move {
                            tokio::task::spawn_blocking(move || {
                                mcsc_core::convert_many(&opts, &list)
                            })
                            .await
                            .unwrap_or_default()
                        },
                        Message::ConvertManyFinished,
                    );
                }
                _ => {}
            }
            app.busy = Some(format!("Converting ({mode})…"));
            app.progress = None;
            app.banner = Some(Banner {
                kind: BannerKind::Working,
                text: format!("Convert started ({mode})…"),
            });
            return Task::perform(
                async move {
                    match tokio::task::spawn_blocking(move || mcsc_core::convert(&opts)).await {
                        Ok(Ok(rep)) => Ok(rep),
                        Ok(Err(e)) => Err(e.to_string()),
                        Err(e) => Err(format!("worker failed: {e}")),
                    }
                },
                Message::ConvertFinished,
            );
        }
        Message::ConvertFinished(res) => {
            app.busy = None;
            app.progress = None;
            match res {
                Ok(rep) => {
                    let summary = if app.dry_run {
                        format!(
                            "Dry run complete: {} steps previewed — no files changed.",
                            rep.actions.len()
                        )
                    } else if !rep.applied {
                        "Convert finished with nothing to apply.".to_string()
                    } else if rep.source_untouched {
                        match &rep.output_root {
                            Some(out) => format!(
                                "Converted to {} in a new copy: {} — source untouched.",
                                app.target.label(),
                                out.display()
                            ),
                            None => {
                                format!("Converted to {} — source untouched.", app.target.label())
                            }
                        }
                    } else {
                        format!(
                            "Converted source in place: {} steps applied, {} backup(s).",
                            rep.actions.len(),
                            rep.backups.len()
                        )
                    };
                    app.push(&summary);
                    for a in &rep.actions {
                        app.push(&format!("  {}", a.describe));
                    }
                    app.banner = Some(Banner {
                        kind: BannerKind::Ok,
                        text: summary.clone(),
                    });
                    app.convert = Some(rep.clone());
                    app.convert_err = None;
                    // Re-verify the result so the UI shows the difference:
                    // pill + STATUS now describe the converted world set.
                    if rep.applied {
                        let (root, level, _) = app.resolve_dirs();
                        let check_root: PathBuf =
                            rep.output_root.clone().unwrap_or_else(|| root.clone());
                        let before = app.detection.as_ref().map(|d| d.layout.label().to_string());
                        detect_into(app, &check_root, &level);
                        // Keep the convert report visible (detect clears it).
                        app.convert = Some(rep.clone());
                        let now = app
                            .detection
                            .as_ref()
                            .map(|d| d.layout.label().to_string())
                            .unwrap_or_else(|| "?".into());
                        let transition = match before {
                            Some(b) => format!("{b} → {now}"),
                            None => now.clone(),
                        };
                        let where_ = match &rep.output_root {
                            Some(out) => format!("new copy: {} (source untouched)", out.display()),
                            None => format!(
                                "in place: {} steps, {} backup(s)",
                                rep.actions.len(),
                                rep.backups.len()
                            ),
                        };
                        let done = format!("Converted {transition} — {where_}.");
                        app.push(&done);
                        app.banner = Some(Banner {
                            kind: BannerKind::Ok,
                            text: done,
                        });
                    }
                }
                Err(e) => {
                    app.push(&format!("Convert failed: {e}"));
                    app.banner = Some(Banner {
                        kind: BannerKind::Err,
                        text: format!("Convert failed: {e}"),
                    });
                    app.convert = None;
                    app.convert_err = Some(e);
                }
            }
        }
        Message::ConvertManyFinished(rep) => {
            app.busy = None;
            app.progress = None;
            let ok = rep.succeeded();
            let failed = rep.failed();
            let total = rep.per_world.len();
            let headline = if app.dry_run {
                format!("Dry run complete for {total} world(s) — no files changed.")
            } else if failed == 0 {
                format!("Converted {ok}/{total} world(s).")
            } else {
                format!("Converted {ok}/{total} world(s), {failed} failed.")
            };
            app.push(&headline);
            for (level, res) in &rep.per_world {
                match res {
                    Ok(r) => {
                        let steps = r.actions.len();
                        let where_ = match &r.output_root {
                            Some(o) => format!("new copy: {}", o.display()),
                            None => "in place".to_string(),
                        };
                        app.push(&format!(
                            "  [{level}] {} ({} steps, {})",
                            if r.applied { "done" } else { "no changes" },
                            steps,
                            where_
                        ));
                    }
                    Err(e) => app.push(&format!("  [{level}] FAILED: {e}")),
                }
            }
            app.many = Some(rep);
            app.convert = None;
            app.convert_err = None;
            // Refresh the primary world's STATUS card (this resets the
            // banner, so restate the many-worlds summary afterwards).
            do_detect(app);
            app.banner = Some(Banner {
                kind: if failed == 0 {
                    BannerKind::Ok
                } else {
                    BannerKind::Err
                },
                text: headline,
            });
        }
    }
    Task::none()
}

// ── View helpers ────────────────────────────────────────

/// Secondary text — Subtext in both Catppuccin flavors. Brighter than
/// Overlay so small text stays readable; still clearly below body text.
fn muted(dark: bool) -> Color {
    if dark {
        Color::from_rgb8(0xba, 0xc2, 0xde)
    } else {
        Color::from_rgb8(0x6c, 0x6f, 0x85)
    }
}

/// Signature accent — Mauve in both flavors.
fn accent(dark: bool) -> Color {
    if dark {
        Color::from_rgb8(0xcb, 0xa6, 0xf7)
    } else {
        Color::from_rgb8(0x88, 0x39, 0xef)
    }
}

/// Catppuccin Green — ok/validation text.
fn ok_green(dark: bool) -> Color {
    if dark {
        Color::from_rgb8(0xa6, 0xe3, 0xa1)
    } else {
        Color::from_rgb8(0x40, 0xa0, 0x2b)
    }
}

/// Catppuccin Red — errors.
fn err_red(dark: bool) -> Color {
    if dark {
        Color::from_rgb8(0xf3, 0x8b, 0xa8)
    } else {
        Color::from_rgb8(0xd2, 0x0f, 0x39)
    }
}

/// Catppuccin Peach — warnings and notes.
fn warn_peach(dark: bool) -> Color {
    if dark {
        Color::from_rgb8(0xfa, 0xb3, 0x87)
    } else {
        Color::from_rgb8(0xfe, 0x64, 0x09)
    }
}

/// Text inputs with a readable placeholder (the theme's is dim).
fn input_style(
    dark: bool,
) -> impl Fn(&iced::Theme, iced::widget::text_input::Status) -> iced::widget::text_input::Style + Copy
{
    move |theme, status| {
        let mut style = iced::widget::text_input::default(theme, status);
        style.placeholder = muted(dark);
        style
    }
}

/// Checkboxes ticked in Mauve instead of the theme's blue. Unchecked
/// boxes keep the theme look; only the ticked fill is overridden.
fn check_style(
) -> impl Fn(&iced::Theme, iced::widget::checkbox::Status) -> iced::widget::checkbox::Style + Copy {
    |theme, status| {
        let mut style = iced::widget::checkbox::primary(theme, status);
        let checked = matches!(
            status,
            iced::widget::checkbox::Status::Active { is_checked: true }
                | iced::widget::checkbox::Status::Hovered { is_checked: true }
        );
        if checked {
            style.background = iced::Background::Color(mauve_fill());
            style.icon_color = Color::WHITE;
            style.border.color = mauve_fill();
        }
        style
    }
}

fn bold() -> iced::Font {
    iced::Font {
        weight: iced::font::Weight::Bold,
        ..iced::Font::DEFAULT
    }
}

/// Fill for primary actions / active targets: deep Mauve (the Latte
/// variant) in both flavors, so the white label is always readable.
fn mauve_fill() -> Color {
    Color::from_rgb8(0x88, 0x39, 0xef)
}

fn section_title(s: &str, dark: bool) -> Element<'_, Message> {
    text(s).size(14).font(bold()).color(accent(dark)).into()
}

fn card<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .padding(18)
        .style(|theme: &iced::Theme| {
            let mut style = container::rounded_box(theme);
            style.border = iced::Border {
                color: theme.extended_palette().secondary.weak.color,
                width: 1.0,
                radius: iced::border::Radius::new(12.0),
            };
            style
        })
        .into()
}

/// Small badge with tinted background, accent border, readable text.
fn badge(
    label: &str,
    bg: (u8, u8, u8),
    edge: (u8, u8, u8),
    fg: Color,
    radius: f32,
) -> Element<'_, Message> {
    container(text(label).size(13).color(fg))
        .padding([4, 12])
        .style(move |_| container::Style {
            background: Some(iced::Background::Color(Color::from_rgb8(bg.0, bg.1, bg.2))),
            border: iced::Border {
                color: Color::from_rgb8(edge.0, edge.1, edge.2),
                width: 1.0,
                radius: iced::border::Radius::new(radius),
            },
            ..container::Style::default()
        })
        .into()
}

fn pill(label: &str, bg: (u8, u8, u8), edge: (u8, u8, u8), fg: Color) -> Element<'_, Message> {
    badge(label, bg, edge, fg, 999.0)
}

fn layout_pill(layout: Option<mcsc_core::Layout>, dark: bool) -> Element<'static, Message> {
    // Tinted chip: theme surface, layout-color border, theme text.
    // Paper modern reads Mauve like every other neutral accent.
    let (green, mauve_edge, peach, gray) = if dark {
        (
            (0xa6, 0xe3, 0xa1),
            (0xcb, 0xa6, 0xf7),
            (0xfa, 0xb3, 0x87),
            (0x6c, 0x70, 0x86),
        )
    } else {
        (
            (0x40, 0xa0, 0x2b),
            (0x88, 0x39, 0xef),
            (0xfe, 0x64, 0x09),
            (0x6c, 0x6f, 0x85),
        )
    };
    let (bg, fg) = if dark {
        ((0x31, 0x32, 0x44), Color::from_rgb8(0xcd, 0xd6, 0xf4))
    } else {
        ((0xe6, 0xe9, 0xef), Color::from_rgb8(0x4c, 0x4f, 0x69))
    };
    match layout {
        None => pill("not detected", bg, gray, fg),
        Some(mcsc_core::Layout::Vanilla) => pill("vanilla pre-26.1", bg, gray, fg),
        Some(mcsc_core::Layout::VanillaModern) => pill("vanilla 26.1+", bg, green, fg),
        Some(mcsc_core::Layout::PaperModern) => pill("paper modern 26.1+", bg, mauve_edge, fg),
        Some(mcsc_core::Layout::LegacySplit) => pill("legacy split", bg, peach, fg),
        Some(mcsc_core::Layout::Unknown) => pill("unknown", bg, gray, fg),
    }
}

fn kv(k: &str, v: String, dark: bool) -> Element<'_, Message> {
    row![
        text(k).size(14).color(muted(dark)).width(130),
        text(v).size(14),
    ]
    .spacing(8)
    .into()
}

/// Live preview of where the converted copy will land.
fn output_preview(app: &App) -> String {
    if app.in_place {
        return "In-place mode: no copy, the source folder itself changes.".to_string();
    }
    let tag = match app.target {
        Target::VanillaModern => "vanilla26",
        Target::Vanilla => "vanilla",
        Target::PaperModern => "papermodern",
        Target::LegacySplit => "legacy",
    };
    let base = app
        .output_base()
        .unwrap_or_else(|| mcsc_core::default_output_base(&app.root_path()))
        .display()
        .to_string();
    let level = if app.level_name.trim().is_empty() {
        "world"
    } else {
        app.level_name.trim()
    };
    format!("Converted copy will be: {base}/{level}-converted-{tag}/")
}

/// Live preview of the offline → online player mapping (or why it won't run).
fn player_mapping_hint(app: &App) -> Element<'_, Message> {
    match resolve_remap(app) {
        Ok(Some((from, to))) => text(format!(
            "Will remap {}… → {}… (dry run lists it, real run copies it).",
            &from[..8],
            &to[..8]
        ))
        .size(13)
        .color(ok_green(app.dark))
        .into(),
        Ok(None) => text("Leave both blank to skip (vanilla servers use online UUIDs).")
            .size(13)
            .color(muted(app.dark))
            .into(),
        Err(e) => text(e).size(13).color(err_red(app.dark)).into(),
    }
}

/// Full-width status banner: working (with live progress), done, or error.
/// This is the answer to "is it done yet" — it stays until the next action.
fn banner_bar(app: &App) -> Option<Element<'_, Message>> {
    let b = app.banner.as_ref()?;
    // Tinted surface + accent border + readable text, per flavor.
    let (bg, edge, fg) = match (b.kind, app.dark) {
        (BannerKind::Working, true) | (BannerKind::Info, true) => (
            (0x31, 0x32, 0x44),
            (0xcb, 0xa6, 0xf7),
            Color::from_rgb8(0xcd, 0xd6, 0xf4),
        ),
        (BannerKind::Working, false) | (BannerKind::Info, false) => (
            (0xe6, 0xe9, 0xef),
            (0x88, 0x39, 0xef),
            Color::from_rgb8(0x4c, 0x4f, 0x69),
        ),
        (BannerKind::Ok, true) => (
            (0x31, 0x32, 0x44),
            (0xa6, 0xe3, 0xa1),
            Color::from_rgb8(0xcd, 0xd6, 0xf4),
        ),
        (BannerKind::Ok, false) => (
            (0xe6, 0xe9, 0xef),
            (0x40, 0xa0, 0x2b),
            Color::from_rgb8(0x4c, 0x4f, 0x69),
        ),
        (BannerKind::Err, true) => (
            (0x31, 0x32, 0x44),
            (0xf3, 0x8b, 0xa8),
            Color::from_rgb8(0xcd, 0xd6, 0xf4),
        ),
        (BannerKind::Err, false) => (
            (0xe6, 0xe9, 0xef),
            (0xd2, 0x0f, 0x39),
            Color::from_rgb8(0x4c, 0x4f, 0x69),
        ),
    };
    let mark: &str = match b.kind {
        BannerKind::Info => "ⓘ",
        BannerKind::Working => "…",
        BannerKind::Ok => "✓",
        BannerKind::Err => "✗",
    };
    let mut line = format!("{mark} {}", b.text);
    if b.kind == BannerKind::Working {
        if let Some(p) = &app.progress {
            if p.total > 0 {
                line.push_str(&format!(" — {} {}/{}", p.label, p.done, p.total));
            } else if !p.label.is_empty() {
                line.push_str(&format!(" — {}", p.label));
            }
        }
    }
    Some(
        container(text(line).size(15).color(fg))
            .padding([10, 16])
            .style(move |_| container::Style {
                background: Some(iced::Background::Color(Color::from_rgb8(bg.0, bg.1, bg.2))),
                border: iced::Border {
                    color: Color::from_rgb8(edge.0, edge.1, edge.2),
                    width: 1.0,
                    radius: iced::border::Radius::new(10.0),
                },
                ..container::Style::default()
            })
            .width(Length::Fill)
            .into(),
    )
}

fn target_button(selected: Target, option: Target, dark: bool) -> Element<'static, Message> {
    let active = selected == option;
    // Active target is deep Mauve with a white label (readable in both
    // flavors; the light Mocha Mauve is text-only).
    let fg = Color::WHITE;
    let hint = if active { fg } else { muted(dark) };
    let inner = column![
        text(option.label()).size(15),
        text(option.hint()).size(13).color(hint),
    ]
    .spacing(2);
    let btn = button(inner)
        .width(Length::Fill)
        .on_press(Message::TargetSelected(option));
    if active {
        let mauve = mauve_fill();
        btn.style(move |theme: &iced::Theme, status| {
            let mut style = button::primary(theme, status);
            style.background = Some(iced::Background::Color(mauve));
            style.text_color = fg;
            style.border = iced::Border {
                color: mauve,
                width: 2.0,
                radius: iced::border::Radius::new(10.0),
            };
            style
        })
        .into()
    } else {
        btn.style(button::secondary).into()
    }
}

// ── View ────────────────────────────────────────────────

fn view(app: &App) -> Element<'_, Message> {
    // Header
    let header = row![
        column![
            text("Minecraft Server World Converter")
                .size(24)
                .font(bold())
                .color(accent(app.dark)),
            text("Purpur · Paper · Spigot · Fabric → Vanilla, and back")
                .size(13)
                .color(muted(app.dark)),
        ]
        .spacing(2),
        space::horizontal(),
        layout_pill(app.detection.as_ref().map(|d| d.layout), app.dark),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let worlds_label = match app.detection.as_ref() {
        Some(d) if d.worlds.len() > 1 => {
            format!("Convert all {} worlds in folder", d.worlds.len())
        }
        _ => "Convert all worlds in folder".to_string(),
    };
    let source_card: Element<'_, Message> = card(
        column![
            section_title("SOURCE", app.dark),
            text("Server folder (contains world/ + server.properties)").size(14),
            row![
                text_input("Pick server folder…", &app.server_root)
                    .on_input(Message::ServerRootChanged)
                    .width(Length::Fill)
                    .style(input_style(app.dark)),
                button(text("Browse").size(14))
                    .on_press(Message::Browse)
                    .style(button::secondary),
            ]
            .spacing(8),
            column![
                text("level-name").size(14),
                text_input("world", &app.level_name)
                    .on_input(Message::LevelNameChanged)
                    .width(180)
                    .style(input_style(app.dark)),
            ]
            .spacing(4),
        ]
        .spacing(10)
        .into(),
    );
    let output_card: Element<'_, Message> = card(
        column![
            section_title("OUTPUT", app.dark),
            row![
                text_input("Next to server folder…", &app.output_dir)
                    .on_input(Message::OutputDirChanged)
                    .width(Length::Fill)
                    .style(input_style(app.dark)),
                button(text("Browse").size(14))
                    .on_press(Message::BrowseOutput)
                    .style(button::secondary),
            ]
            .spacing(8),
            text(output_preview(app)).size(13).color(muted(app.dark)),
        ]
        .spacing(10)
        .into(),
    );
    let target_card: Element<'_, Message> = card(
        column![
            section_title("TARGET", app.dark),
            target_button(app.target, Target::VanillaModern, app.dark),
            target_button(app.target, Target::Vanilla, app.dark),
            target_button(app.target, Target::PaperModern, app.dark),
            target_button(app.target, Target::LegacySplit, app.dark),
            checkbox(app.convert_all)
                .style(check_style())
                .label(worlds_label)
                .on_toggle(Message::ConvertAllToggled),
        ]
        .spacing(10)
        .into(),
    );
    let options_card: Element<'_, Message> = card(
        column![
            row![
                section_title("OPTIONS", app.dark),
                space::horizontal(),
                button(text(if app.dark { "● Mocha" } else { "○ Latte" }).size(13))
                    .on_press(Message::ToggleTheme)
                    .style(button::secondary),
            ]
            .align_y(iced::Alignment::Center),
            checkbox(app.backup)
                .style(check_style())
                .label("Backup before converting")
                .on_toggle(Message::BackupToggled),
            checkbox(app.dry_run)
                .style(check_style())
                .label("Dry run (preview only)")
                .on_toggle(Message::DryRunToggled),
            checkbox(app.overwrite)
                .style(check_style())
                .label("Overwrite destination")
                .on_toggle(Message::OverwriteToggled),
            checkbox(app.in_place)
                .style(check_style())
                .label("Convert in place (modify source)")
                .on_toggle(Message::InPlaceToggled),
        ]
        .spacing(10)
        .into(),
    );
    let player_card: Element<'_, Message> = card(
        column![
            section_title("PLAYER (offline → online)", app.dark),
            checkbox(app.auto_remap).style(check_style())
                .label("Auto-remap all players (look up online UUIDs)")
                .on_toggle(Message::AutoRemapToggled),
            text("Uses names from usercache.json, ops, whitelist; same-name offline→online pairs match locally, rest need internet, unknown names are skipped.")
                .size(13)
                .color(muted(app.dark)),
            text("Server ran offline-mode? Carry inventory + position over:")
                .size(13)
                .color(muted(app.dark)),
            text_input("Offline username (on the server)", &app.player_name)
                .on_input(Message::PlayerNameChanged)
                .width(Length::Fill)
                .style(input_style(app.dark)),
            text_input("Online UUID (your account now)", &app.online_uuid)
                .on_input(Message::OnlineUuidChanged)
                .width(Length::Fill)
                .style(input_style(app.dark)),
            player_mapping_hint(app),
        ]
        .spacing(10)
        .into(),
    );
    let left: Element<'_, Message> = container(
        scrollable(
            column![
                source_card,
                output_card,
                target_card,
                player_card,
                options_card
            ]
            .spacing(12),
        )
        .spacing(10)
        .height(Length::Fill),
    )
    .width(Length::FillPortion(5))
    .height(Length::Fill)
    .into();

    // Right: status + reports
    let mut right_col = column![].spacing(12);

    // Status card
    let status_body: Element<'_, Message> = match (&app.detection, &app.level_info, &app.level_err)
    {
        (None, _, _) => text("No world detected yet.").size(15).into(),
        (Some(det), info, err) => {
            let mut c = column![
                kv("Layout", det.layout.label().to_string(), app.dark),
                kv(
                    "World",
                    format!("{}/{}", det.server_root.display(), det.level_name),
                    app.dark
                ),
            ]
            .spacing(4);
            if let Some(i) = info {
                c = c.push(kv(
                    "level.dat",
                    format!("DataVersion={:?} Name={:?}", i.data_version, i.level_name),
                    app.dark,
                ));
            }
            if let Some(e) = err {
                c = c.push(
                    text(format!("level.dat: {e}"))
                        .size(14)
                        .color(err_red(app.dark)),
                );
            }
            for n in &det.notes {
                c = c.push(text(format!("ⓘ {n}")).size(13).color(warn_peach(app.dark)));
            }
            if !det.worlds.is_empty() {
                c = c.push(section_title("WORLDS IN FOLDER", app.dark));
                for w in &det.worlds {
                    let mut line = format!("• {} ({})", w.level_name, w.layout.label());
                    if !w.mirrors.is_empty() {
                        line.push_str(&format!(" + {}", w.mirrors.join(", ")));
                    }
                    c = c.push(text(line).size(13));
                }
            }
            c.into()
        }
    };
    right_col = right_col.push(card(
        column![section_title("STATUS", app.dark), status_body]
            .spacing(8)
            .into(),
    ));

    // Scan card — only shown once a scan exists.
    if let Some(rep) = &app.scan {
        {
            let mut c = column![text(format!(
                "{} files · {} chunks · {} with modded IDs",
                rep.files_scanned, rep.chunks_scanned, rep.chunks_with_modded
            ))
            .size(14)]
            .spacing(6);
            if rep.namespaces.is_empty() {
                c = c.push(text("✓ No non-vanilla namespaces — vanilla-safe.").size(14));
            } else {
                for (ns, n) in rep.namespaces.iter().take(12) {
                    c = c.push(row![
                        text(ns.clone()).size(14).width(Length::Fill),
                        text(format!("{n}")).size(14),
                    ]);
                }
                if !rep.top_ids.is_empty() {
                    c = c.push(section_title("TOP IDS", app.dark));
                    for (id, n) in rep.top_ids.iter().take(10) {
                        c = c.push(row![
                            text(id.clone()).size(13).width(Length::Fill),
                            text(format!("{n}")).size(13),
                        ]);
                    }
                }
            }
            for w in &rep.warnings {
                c = c.push(text(w.clone()).size(13).color(muted(app.dark)));
            }
            let scan_body: Element<'_, Message> = c.into();
            right_col = right_col.push(card(
                column![section_title("MOD SCAN (REPORT ONLY)", app.dark), scan_body]
                    .spacing(8)
                    .into(),
            ));
        }
    }

    // Convert card
    let convert_body: Element<'_, Message> = if let Some(many) = &app.many {
        let mut c = column![].spacing(4);
        let mut shown = 0usize;
        for (level, res) in &many.per_world {
            match res {
                Ok(rep) => {
                    let status = if rep.applied { "done" } else { "no changes" };
                    c = c.push(text(format!("—— {level}: {status} ——")).size(14));
                    for a in rep.actions.iter().take(8) {
                        if shown >= 40 {
                            break;
                        }
                        c = c.push(text(format!("• {}", a.describe)).size(13));
                        shown += 1;
                    }
                }
                Err(e) => {
                    c = c.push(
                        text(format!("—— {level}: FAILED: {e} ——"))
                            .size(14)
                            .color(err_red(app.dark)),
                    );
                }
            }
        }
        c.into()
    } else {
        match (&app.convert, &app.convert_err) {
            (None, None) => column![].spacing(4).into(),
            (None, Some(e)) => text(format!("Failed: {e}"))
                .size(14)
                .color(err_red(app.dark))
                .into(),
            (Some(rep), _) => {
                let mut c = column![].spacing(4);
                if rep.source_untouched {
                    match &rep.output_root {
                        Some(out) => {
                            c = c.push(
                                text(format!("⧉ Output (new copy): {}", out.display())).size(14),
                            );
                            c = c.push(
                                text("Source folder was not modified.")
                                    .size(13)
                                    .color(ok_green(app.dark)),
                            );
                        }
                        None => {
                            c = c.push(
                                text("Source folder was not modified.")
                                    .size(13)
                                    .color(ok_green(app.dark)),
                            );
                        }
                    }
                } else if let Some(out) = &rep.output_root {
                    c = c.push(text(format!("⧉ Output: {}", out.display())).size(14));
                }
                for a in rep.actions.iter().take(30) {
                    c = c.push(text(format!("• {}", a.describe)).size(13));
                }
                if rep.actions.len() > 30 {
                    c = c.push(
                        text(format!("… +{} more", rep.actions.len() - 30))
                            .size(13)
                            .color(muted(app.dark)),
                    );
                }
                for w in &rep.warnings {
                    c = c.push(text(format!("⚠ {w}")).size(13).color(warn_peach(app.dark)));
                }
                for b in &rep.backups {
                    c = c.push(text(format!("⧉ {}", b.display())).size(13));
                }
                c.into()
            }
        }
    };
    if app.many.is_some() || app.convert.is_some() || app.convert_err.is_some() {
        right_col = right_col.push(card(
            column![
                section_title(
                    if app.dry_run {
                        "CONVERT PREVIEW"
                    } else {
                        "CONVERT RESULT"
                    },
                    app.dark
                ),
                convert_body
            ]
            .spacing(8)
            .into(),
        ));
    }

    if !app.log.is_empty() {
        right_col = right_col.push(card(
            column![
                section_title("LOG", app.dark),
                scrollable(text(&app.log).size(12).font(iced::Font::MONOSPACE)).height(120),
            ]
            .spacing(8)
            .into(),
        ));
    }

    let right: Element<'_, Message> =
        container(scrollable(right_col.spacing(12)).height(Length::Fill))
            .width(Length::FillPortion(7))
            .height(Length::Fill)
            .into();

    let main_row = row![left, right].spacing(16).height(Length::Fill);

    // Footer actions
    let busy_label: Element<'_, Message> = match (&app.busy, &app.progress) {
        (Some(_), Some(p)) if p.total > 0 => text(format!("{} {}/{}", p.label, p.done, p.total))
            .size(14)
            .color(accent(app.dark))
            .into(),
        (Some(_), Some(p)) => text(p.label.clone())
            .size(14)
            .color(accent(app.dark))
            .into(),
        (Some(b), None) => text(b.clone()).size(14).color(accent(app.dark)).into(),
        (None, _) => text("").size(13).into(),
    };
    // Primary actions wear the deep Mauve fill with a white label.
    // In-place stays danger-red: it modifies the source.
    let mauve = mauve_fill();
    let mauve_fg = Color::WHITE;
    let convert_btn = if app.dry_run {
        button(text("Convert (dry run)").size(14))
            .on_press(Message::Convert)
            .style(move |theme: &iced::Theme, status| {
                let mut style = button::primary(theme, status);
                style.background = Some(iced::Background::Color(mauve));
                style.text_color = mauve_fg;
                style
            })
    } else if app.in_place {
        button(text("Convert in place").size(14))
            .on_press(Message::Convert)
            .style(button::danger)
    } else {
        button(text("Convert to copy").size(14))
            .on_press(Message::Convert)
            .style(move |theme: &iced::Theme, status| {
                let mut style = button::primary(theme, status);
                style.background = Some(iced::Background::Color(mauve));
                style.text_color = mauve_fg;
                style
            })
    };
    let footer = row![
        button(text("Detect").size(14))
            .on_press(Message::Detect)
            .style(button::secondary),
        button(text("Scan mods").size(14))
            .on_press(Message::Scan)
            .style(button::secondary),
        convert_btn,
        button(text("Clear").size(14))
            .on_press(Message::Clear)
            .style(button::text),
        space::horizontal(),
        busy_label,
    ]
    .spacing(10)
    .align_y(iced::Alignment::Center);

    let mut page = column![header, rule::horizontal(1)];
    if let Some(bar) = banner_bar(app) {
        page = page.push(bar);
    }
    let page = page
        .push(main_row)
        .push(footer)
        .spacing(16)
        .padding(24)
        .width(Length::Fill);

    container(page)
        .center_x(Length::Fill)
        .width(Length::Fill)
        .into()
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
        app.server_root = server_root.to_string();
        app.level_name = level_name.to_string();
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
        assert_eq!(app.server_root, before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_base_empty_means_default() {
        let mut app = app_with("/srv/mc", "world");
        assert_eq!(app.output_base(), None);
        app.output_dir = "   ".to_string();
        assert_eq!(app.output_base(), None);
        app.output_dir = "/tmp/out".to_string();
        assert_eq!(app.output_base(), Some(PathBuf::from("/tmp/out")));
    }

    #[test]
    fn resolve_remap_needs_both_fields() {
        let mut app = app_with("/srv/mc", "world");
        // Blank pair skips.
        assert_eq!(resolve_remap(&app), Ok(None));
        // Only one side set explains what's missing.
        app.player_name = "Anivia31".to_string();
        assert!(resolve_remap(&app).is_err());
        app.player_name.clear();
        app.online_uuid = "10b8a89c-bb59-4113-a62f-5ac96b915dd9".to_string();
        assert!(resolve_remap(&app).is_err());
        // Bad UUID shape is rejected.
        app.player_name = "Anivia31".to_string();
        app.online_uuid = "nope".to_string();
        assert!(resolve_remap(&app).is_err());
        // Real pair resolves through the offline-UUID derivation.
        app.online_uuid = "10b8a89c-bb59-4113-a62f-5ac96b915dd9".to_string();
        assert_eq!(
            resolve_remap(&app),
            Ok(Some((
                "5ad08d99-26a2-331c-8f77-d476dec25826".to_string(),
                "10b8a89c-bb59-4113-a62f-5ac96b915dd9".to_string()
            )))
        );
    }
}
