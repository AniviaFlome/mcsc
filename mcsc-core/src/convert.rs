use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::backup::backup_world_dir;
use crate::detect::{detect_layout, Layout};
use crate::players::{is_uuid, offline_uuid, read_identities, uuid_version, UuidResolver};
use crate::progress::ProgressReporter;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TargetLayout {
    /// Vanilla / Fabric / Forge 26.1+: unified dimensions/ layout.
    /// Minimal, safe: dat placement, seed file, player remap. Terrain stays.
    VanillaModern,
    /// Vanilla / Fabric / Forge pre-26.1: legacy Anvil layout (top-level
    /// region/ + DIM-1/DIM1, seed inside level.dat).
    Vanilla,
    /// Paper modern 26.1+ and forks (same dimensions/ tree as VanillaModern).
    PaperModern,
    /// Spigot / CraftBukkit split layout.
    LegacySplit,
}

impl TargetLayout {
    pub fn label(self) -> &'static str {
        match self {
            TargetLayout::VanillaModern => "Vanilla 26.1+ (dimensions layout)",
            TargetLayout::Vanilla => "Vanilla pre-26.1 (legacy Anvil)",
            TargetLayout::PaperModern => "Paper modern",
            TargetLayout::LegacySplit => "Legacy split (Spigot)",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub server_root: PathBuf,
    pub level_name: String,
    pub target: TargetLayout,
    /// Create `.bak-<timestamp>` copies before touching anything (in-place
    /// mode only; copy mode never touches the source).
    pub backup: bool,
    /// Don't write; only report what would happen.
    pub dry_run: bool,
    /// Allow overwriting existing destination DIM folders / dat files, or an
    /// existing output folder in copy mode.
    pub overwrite: bool,
    /// Optional phase updates ("Backing up world…"). Called from the worker
    /// thread; keep it cheap and non-blocking.
    pub progress: Option<ProgressReporter>,
    /// false (default, safe): copy the world(s) to a new output folder and
    /// convert the copy — the source is never modified. true: convert the
    /// source folder directly, in place.
    pub in_place: bool,
    /// Optional player remap (offline UUID -> online UUID) for servers that
    /// ran in offline mode: copies playerdata/stats/advancements to the new
    /// name so inventories, positions and progress carry over. Planned in
    /// dry runs, applied in real runs.
    pub uuid_remap: Option<(String, String)>,
    /// When true, every username in the server's usercache.json whose offline
    /// player file exists is looked up (via `uuid_resolver`) and remapped
    /// automatically — no typing, covers all players. Unknown or unreachable
    /// names are skipped with a note.
    pub auto_remap_all: bool,
    /// Online-UUID lookup used by `auto_remap_all`. Runs on the worker thread.
    pub uuid_resolver: Option<UuidResolver>,
    /// Where the converted copy goes in copy mode. `None` (default) means
    /// next to the source, inside the server folder.
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertAction {
    pub describe: String,
    pub from: Option<PathBuf>,
    pub to: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertReport {
    pub actions: Vec<ConvertAction>,
    pub warnings: Vec<String>,
    pub backups: Vec<PathBuf>,
    /// Set when files were actually written (false for dry runs and no-ops).
    pub applied: bool,
    /// Where the converted world set lives. `None` in dry-run preview of
    /// copy mode still carries the *planned* path.
    pub output_root: Option<PathBuf>,
    /// True when the source folders were not modified.
    pub source_untouched: bool,
}

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Refusing to overwrite existing {0} (enable overwrite to replace)")]
    Exists(String),
    #[error("Nothing to convert: {0}")]
    NothingToDo(String),
}

pub fn convert(opts: &ConvertOptions) -> Result<ConvertReport, ConvertError> {
    let mut report = ConvertReport::default();
    let root = &opts.server_root;
    let level = root.join(&opts.level_name);

    if !level.is_dir() {
        return Err(ConvertError::NothingToDo(format!(
            "world folder not found: {}",
            level.display()
        )));
    }

    // Decide where the conversion happens. Copy mode leaves the source
    // untouched; in-place mode works directly on it.
    let mut say = make_say(opts);
    let work_root: PathBuf;
    // Output base: explicit choice, else next to the server folder (never
    // inside it — converted copies sit beside `server/`, not polluting it).
    let out_base: PathBuf = match &opts.output_dir {
        Some(d) if !d.as_os_str().is_empty() => d.clone(),
        _ => default_output_base(root),
    };
    if opts.in_place {
        work_root = root.clone();
        report.source_untouched = false;
    } else {
        let out = plan_output_dir(&out_base, &opts.level_name, opts.target, opts.overwrite)?;
        // The converted copy must live next to the world, never inside it:
        // copying a world into itself would recurse and swallow the source.
        if out == level || out.starts_with(&level) {
            return Err(ConvertError::NothingToDo(format!(
                "output folder {} is inside the world folder {}; pick a folder next to the world instead",
                out.display(),
                level.display()
            )));
        }
        report.output_root = Some(out.clone());
        report.source_untouched = true;
        copy_world_set(root, &out, &opts.level_name, opts, &mut report, &mut say)?;
        work_root = out;
    }
    let work_opts = ConvertOptions {
        server_root: work_root.clone(),
        // Backups only make sense when the source itself is mutated.
        backup: opts.backup && opts.in_place,
        // Within a fresh copy nothing of value can be clobbered (the source
        // is the safety net), so intra-copy moves always may replace stale
        // files. The previous-output-dir check above still honors the flag.
        overwrite: opts.overwrite || !opts.in_place,
        ..opts.clone()
    };
    // In dry-run copy mode the output dir is never created, so plan sources
    // from the original root; otherwise plan within the working root.
    let plan_src: PathBuf = if !opts.in_place && opts.dry_run {
        root.clone()
    } else {
        work_root
    };

    // Player remaps: the manual pair plus auto-discovered ones (resolved
    // here so dry runs preview them too).
    let mut remaps: Vec<(String, String)> = opts.uuid_remap.clone().into_iter().collect();
    if opts.auto_remap_all {
        let (auto_pairs, unmapped) = discover_auto_pairs(
            root,
            &plan_src.join(&opts.level_name),
            opts.uuid_resolver.as_ref(),
            &mut say,
        );
        remaps.extend(auto_pairs);
        for uuid in unmapped {
            // Manual pair may still cover it; only warn when truly orphaned.
            let covered = remaps.iter().any(|(f, _)| f == &uuid);
            if !covered {
                report.warnings.push(format!(
                    "No username known for player file {uuid} — tell me whose it is to remap it."
                ));
            }
        }
    }

    let did_plan = match work_opts.target {
        TargetLayout::VanillaModern => {
            // 26.1+ everywhere shares one unified layout; only normalize the
            // legacy ones and place files for the modern ones.
            match detect_layout(&opts.server_root, &opts.level_name).layout {
                Layout::LegacySplit | Layout::Vanilla => {
                    report
                        .warnings
                        .push("Legacy layout normalized to the modern unified layout.".into());
                    normalize_to_modern(&work_opts, &mut report, &mut say, &plan_src, &remaps)?
                }
                _ => minimal_modern(&work_opts, &mut report, &mut say, &plan_src, &remaps)?,
            }
        }
        TargetLayout::Vanilla => {
            convert_to_vanilla(&work_opts, &mut report, &mut say, &plan_src, &remaps)?
        }
        TargetLayout::PaperModern => {
            // Paper adopts a Vanilla layout on first boot, so there is
            // nothing to move — except legacy layouts, which normalize to
            // the modern unified tree Paper reads natively.
            let src_layout = detect_layout(&opts.server_root, &opts.level_name).layout;
            if matches!(src_layout, Layout::LegacySplit | Layout::Vanilla) {
                report.warnings.push(
                    "Legacy layout normalized to the modern unified layout Paper reads natively."
                        .into(),
                );
                normalize_to_modern(&work_opts, &mut report, &mut say, &plan_src, &remaps)?
            } else {
                report.warnings.push(
                    "Already in the modern unified layout: Paper adopts it on first boot. No changes made.".into(),
                );
                false
            }
        }
        TargetLayout::LegacySplit => {
            // Pre-26.1 servers cannot read 26.2+ chunks at all; still
            // normalize modern sources to a portable layout rather than
            // refusing, and say so plainly.
            match detect_layout(&opts.server_root, &opts.level_name).layout {
                Layout::PaperModern | Layout::VanillaModern => {
                    report.warnings.push(
                        "Spigot split targets pre-26.1 servers, which cannot load 26.2+ chunks; normalized to the portable modern layout instead.".into(),
                    );
                    normalize_to_modern(&work_opts, &mut report, &mut say, &plan_src, &remaps)?
                }
                _ => convert_to_legacy(&work_opts, &mut report, &mut say, &plan_src)?,
            }
        }
    };
    report.applied = did_plan && !opts.dry_run;

    Ok(report)
}

/// Discover (offline_uuid, online_uuid) pairs automatically, without typing:
/// every username known to the server (usercache.json, ops.json,
/// whitelist.json, banned-players.json) whose offline player file exists is
/// paired — directly when the same name already holds both a v3 (offline)
/// and a v4 (online) UUID locally, otherwise via `resolver` lookup.
/// Files with no known username come back as `unmapped` so the report can
/// name them instead of silently dropping them. Nothing here ever fails the
/// convert.
#[allow(clippy::too_many_arguments)]
fn discover_auto_pairs(
    orig_server_root: &Path,
    plan_level: &Path,
    resolver: Option<&UuidResolver>,
    say: &mut dyn FnMut(String),
) -> (Vec<(String, String)>, Vec<String>) {
    // Offline player files actually present (vanilla + Paper layouts).
    let mut present = std::collections::HashSet::new();
    for dir in [
        plan_level.join("playerdata"),
        plan_level.join("players").join("data"),
    ] {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.filter_map(|e| e.ok()) {
            if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                if is_uuid(stem) {
                    present.insert(stem.to_string());
                }
            }
        }
    }
    if present.is_empty() {
        return (Vec::new(), Vec::new());
    }
    // Group known UUIDs per username across all local identity sources.
    let mut by_name: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (name, uuid) in read_identities(orig_server_root) {
        if is_uuid(&uuid) {
            by_name.entry(name).or_default().push(uuid);
        }
    }
    let mut pairs = Vec::new();
    let mut covered = std::collections::HashSet::new();
    for (name, uuids) in &by_name {
        let offline = offline_uuid(name);
        if !present.contains(&offline) {
            continue;
        }
        // Same name holding both flavors locally: certain match, no network.
        if let Some(online) = uuids
            .iter()
            .find(|u| uuid_version(u) == Some('4') && **u != offline)
        {
            say(format!("Paired {name} locally (no lookup needed)…"));
            pairs.push((offline.clone(), online.clone()));
            covered.insert(offline);
            continue;
        }
        // Otherwise fall back to the online lookup.
        let Some(resolver) = resolver else {
            continue;
        };
        say(format!("Looking up online UUID for {name}…"));
        match resolver.resolve(name) {
            Some(online) if is_uuid(&online) && online != offline => {
                pairs.push((offline.clone(), online));
                covered.insert(offline);
            }
            _ => say(format!("No online UUID found for {name} — skipped.")),
        }
    }
    // Offline files nobody claims: name them so the user can identify them.
    let mut unmapped: Vec<String> = present.difference(&covered).cloned().collect();
    unmapped.sort();
    (pairs, unmapped)
}

/// Result of converting several worlds in one run.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConvertManyReport {
    /// Per world, in the requested order: Ok(report) or Err(message).
    pub per_world: Vec<(String, Result<ConvertReport, String>)>,
}

impl ConvertManyReport {
    pub fn succeeded(&self) -> usize {
        self.per_world
            .iter()
            .filter(|(_, r)| matches!(r, Ok(rep) if rep.applied))
            .count()
    }

    pub fn failed(&self) -> usize {
        self.per_world.iter().filter(|(_, r)| r.is_err()).count()
    }
}

/// Convert several worlds with the same options (level_name replaced per
/// world). Each world is independent: one world's failure is collected and
/// the rest still run. Progress labels are prefixed with the world name.
pub fn convert_many(base: &ConvertOptions, levels: &[String]) -> ConvertManyReport {
    let mut out = ConvertManyReport::default();
    for level in levels {
        let mut opts = base.clone();
        opts.level_name = level.clone();
        if let Some(p) = &base.progress {
            let p = p.clone();
            let w = level.clone();
            opts.progress = Some(ProgressReporter::new(move |pr| {
                p.report(pr.done, pr.total, format!("[{w}] {}", pr.label));
            }));
        }
        let res = convert(&opts).map_err(|e| e.to_string());
        out.per_world.push((level.clone(), res));
    }
    out
}

/// Default output base in copy mode: the parent of the server folder, so
/// converted copies land next to `server/` rather than inside it.
pub fn default_output_base(server_root: &Path) -> PathBuf {
    server_root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| server_root.to_path_buf())
}

/// Output folder for copy mode: `<base>/<level>-converted-<target>/`.
fn plan_output_dir(
    root: &Path,
    level_name: &str,
    target: TargetLayout,
    overwrite: bool,
) -> Result<PathBuf, ConvertError> {
    let tag = match target {
        TargetLayout::VanillaModern => "vanilla26",
        TargetLayout::Vanilla => "vanilla",
        TargetLayout::PaperModern => "papermodern",
        TargetLayout::LegacySplit => "legacy",
    };
    let out = root.join(format!("{level_name}-converted-{tag}"));
    if out.exists() && !overwrite {
        return Err(ConvertError::Exists(format!(
            "{} (output from a previous convert)",
            out.display()
        )));
    }
    Ok(out)
}

/// Copy the world set (level dir + legacy mirrors + server.properties)
/// into a fresh output dir. Reports file-count progress.
fn copy_world_set(
    root: &Path,
    out: &Path,
    level_name: &str,
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
) -> Result<(), ConvertError> {
    if out.exists() {
        if opts.dry_run {
            // Preview only: nothing to remove yet.
        } else {
            say(format!(
                "Removing previous output {}…",
                out.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            ));
            fs::remove_dir_all(out)?;
            report.actions.push(ConvertAction {
                describe: format!("Remove previous output {}", out.display()),
                from: None,
                to: Some(out.to_path_buf()),
            });
        }
    }

    let mut sources: Vec<PathBuf> = vec![root.join(level_name)];
    for extra in [
        root.join(format!("{level_name}_nether")),
        root.join(format!("{level_name}_the_end")),
    ] {
        if extra.is_dir() {
            sources.push(extra);
        }
    }
    if root.join("server.properties").is_file() {
        sources.push(root.join("server.properties"));
    }

    // Count files upfront so progress has a total.
    let mut total = 0u64;
    if !opts.dry_run {
        for src in &sources {
            total += count_files(src);
        }
    }
    let mut done = 0u64;
    for src in &sources {
        let name = src
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let dst = out.join(&name);
        let action = format!("Copy {} -> {}", src.display(), dst.display());
        if opts.dry_run {
            report.actions.push(ConvertAction {
                describe: action,
                from: Some(src.clone()),
                to: Some(dst),
            });
            continue;
        }
        say(format!("Copying {name}…"));
        if let Some(p) = &opts.progress {
            p.report(done, total.max(1), format!("Copying {name}…"));
        }
        copy_with_progress(src, &dst, &mut done, total, opts.progress.as_ref())?;
        report.actions.push(ConvertAction {
            describe: action,
            from: Some(src.clone()),
            to: Some(dst),
        });
    }
    if !opts.dry_run {
        if let Some(p) = &opts.progress {
            p.report(total.max(1), total.max(1), "Copy complete.");
        }
    }
    Ok(())
}

fn count_files(path: &Path) -> u64 {
    if path.is_file() {
        return 1;
    }
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .count() as u64
}

fn copy_with_progress(
    src: &Path,
    dst: &Path,
    done: &mut u64,
    total: u64,
    progress: Option<&ProgressReporter>,
) -> std::io::Result<()> {
    if src.is_file() {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dst)?;
        *done += 1;
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    let mut entries: Vec<_> = fs::read_dir(src)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_progress(&entry.path(), &to, done, total, progress)?;
        } else if ty.is_file() {
            fs::copy(entry.path(), &to)?;
            *done += 1;
            if (*done).is_multiple_of(64) {
                if let Some(p) = progress {
                    p.report(*done, total.max(1), "Copying world…".to_string());
                }
            }
        }
    }
    Ok(())
}

fn copy_dir_progress(
    src: &Path,
    dst: &Path,
    done: &mut u64,
    total: u64,
    progress: Option<&ProgressReporter>,
) -> std::io::Result<()> {
    copy_with_progress(src, dst, done, total, progress)
}

fn make_say<'a>(opts: &'a ConvertOptions) -> impl FnMut(String) + 'a {
    let mut step = 0u64;
    move |label: String| {
        step += 1;
        if let Some(p) = &opts.progress {
            // total unknown for moves; UI shows label + spinner.
            p.report(step, 0, label);
        }
    }
}

/// Would `dst` exist at execution time? In dry-run copy mode the output dir
/// is never created, so a plain `exists()` check misses collisions that the
/// copy would create. Checking the source counterpart keeps dry-run previews
/// faithful: collisions surface in the preview, not mid-way through the
/// real run.
fn dst_conflicts(dst: &Path, opts: &ConvertOptions, plan_src_root: &Path) -> bool {
    if dst.exists() {
        return true;
    }
    if !opts.in_place && opts.dry_run {
        if let Ok(rel) = dst.strip_prefix(&opts.server_root) {
            return plan_src_root.join(rel).exists();
        }
    }
    false
}

fn maybe_backup(
    dir: &Path,
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
) -> std::io::Result<()> {
    if opts.backup && !opts.dry_run && dir.exists() {
        say(format!(
            "Backing up {}…",
            dir.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| dir.display().to_string())
        ));
        let bak = backup_world_dir(dir)?;
        report.backups.push(bak.clone());
        report.actions.push(ConvertAction {
            describe: format!("Backup {} -> {}", dir.display(), bak.display()),
            from: Some(dir.to_path_buf()),
            to: Some(bak),
        });
    }
    Ok(())
}

/// Move the *contents* of one store dir (region/entities/poi) into another,
/// merging file by file so a partially-present destination survives.
/// Removes the emptied source dir on real runs. Returns true if anything
/// was planned (also true for dry runs).
fn merge_store_dir(
    src: &Path,
    dst: &Path,
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
    what: &str,
    plan_src: &Path,
) -> Result<bool, ConvertError> {
    if !src.is_dir() {
        return Ok(false);
    }
    let mut entries: Vec<_> = fs::read_dir(src)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    if entries.is_empty() {
        return Ok(false);
    }
    entries.sort();
    say(format!("Moving {what} ({} files)…", entries.len()));
    let mut moved_any = false;
    for src_path in entries {
        let name = src_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if src_path.is_dir() {
            // Recurse for unexpected nesting.
            let sub_dst = dst.join(&name);
            if merge_store_dir(&src_path, &sub_dst, opts, report, say, &name, plan_src)? {
                moved_any = true;
            }
            continue;
        }
        let dst_path = dst.join(&name);
        if dst_conflicts(&dst_path, opts, plan_src) {
            if !opts.overwrite {
                return Err(ConvertError::Exists(dst_path.display().to_string()));
            }
            report.actions.push(ConvertAction {
                describe: format!("Replace existing {}", dst_path.display()),
                from: None,
                to: Some(dst_path.clone()),
            });
            if !opts.dry_run && dst_path.exists() {
                fs::remove_file(&dst_path)?;
            }
        }
        report.actions.push(ConvertAction {
            describe: format!(
                "Move {} {} -> {}",
                what,
                src_path.display(),
                dst_path.display()
            ),
            from: Some(src_path.clone()),
            to: Some(dst_path.clone()),
        });
        if !opts.dry_run {
            if let Some(parent) = dst_path.parent() {
                fs::create_dir_all(parent)?;
            }
            if dst_path.exists() {
                fs::remove_file(&dst_path)?;
            }
            if fs::rename(&src_path, &dst_path).is_err() {
                fs::copy(&src_path, &dst_path)?;
                fs::remove_file(&src_path)?;
            }
        }
        moved_any = true;
    }
    if moved_any && !opts.dry_run {
        // Remove the emptied source dir so the tree reads clean.
        let still_there = fs::read_dir(src)
            .map(|mut it| it.next().is_some())
            .unwrap_or(true);
        if !still_there {
            fs::remove_dir(src)?;
            report.actions.push(ConvertAction {
                describe: format!("Remove empty {}", src.display()),
                from: None,
                to: Some(src.to_path_buf()),
            });
        }
    }
    Ok(moved_any)
}

/// Copy one player's data/stats/advancements from an offline UUID name to an
/// online UUID name (keeping the offline originals as fallback).
/// `modern` selects the 26.1+ `players/` tree, otherwise the legacy
/// `playerdata/` tree. Returns true when anything was planned.
/// Sources prefer the working tree, then the Paper `players/` tree and the
/// legacy `playerdata/` tree (covers dry-run copy mode, where the working
/// tree does not exist yet).
#[allow(clippy::too_many_arguments)]
fn remap_player_files(
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
    level: &Path,
    src_level: &Path,
    from_uuid: &str,
    to_uuid: &str,
    modern: bool,
) -> Result<bool, ConvertError> {
    if from_uuid == to_uuid {
        return Ok(false);
    }
    // (destination subdir parts, source filename, destination filename).
    // Destination always lives in the working tree; every layout that could
    // hold the offline original is tried as source below.
    let sets: [(Vec<&str>, String, String); 3] = if modern {
        [
            (
                vec!["players", "data"],
                format!("{from_uuid}.dat"),
                format!("{to_uuid}.dat"),
            ),
            (
                vec!["players", "stats"],
                format!("{from_uuid}.json"),
                format!("{to_uuid}.json"),
            ),
            (
                vec!["players", "advancements"],
                format!("{from_uuid}.json"),
                format!("{to_uuid}.json"),
            ),
        ]
    } else {
        [
            (
                vec!["playerdata"],
                format!("{from_uuid}.dat"),
                format!("{to_uuid}.dat"),
            ),
            (
                vec!["stats"],
                format!("{from_uuid}.json"),
                format!("{to_uuid}.json"),
            ),
            (
                vec!["advancements"],
                format!("{from_uuid}.json"),
                format!("{to_uuid}.json"),
            ),
        ]
    };
    let mut planned = false;
    for (sub, from_file, to_file) in &sets {
        let mut dst = level.to_path_buf();
        for p in sub {
            dst = dst.join(p);
        }
        let dst = dst.join(to_file);
        // Every layout that could hold the offline original.
        let mut candidates = vec![dst.parent().unwrap().join(from_file)];
        for base in [src_level, level] {
            candidates.push(base.join("players/data").join(from_file));
            candidates.push(base.join("players/stats").join(from_file));
            candidates.push(base.join("players/advancements").join(from_file));
            candidates.push(base.join("playerdata").join(from_file));
            candidates.push(base.join("stats").join(from_file));
            candidates.push(base.join("advancements").join(from_file));
        }
        let Some(src) = candidates.into_iter().find(|p| p.is_file()) else {
            continue;
        };
        // Remap destinations are fresh online-UUID names; a clash only
        // happens on re-runs, where the flag (or copy mode) governs it.
        if dst.exists() && !opts.overwrite {
            return Err(ConvertError::Exists(dst.display().to_string()));
        }
        say(format!(
            "Remapping player {} → {}…",
            from_uuid.get(..8).unwrap_or(from_uuid),
            to_uuid.get(..8).unwrap_or(to_uuid)
        ));
        report.actions.push(ConvertAction {
            describe: format!("Remap player {} -> {}", src.display(), dst.display()),
            from: Some(src.clone()),
            to: Some(dst.clone()),
        });
        if !opts.dry_run {
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src, &dst)?;
        }
        planned = true;
    }
    Ok(planned)
}

fn move_or_copy_dir(
    src: &Path,
    dst: &Path,
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    what: &str,
    say: &mut dyn FnMut(String),
    plan_src: &Path,
) -> Result<(), ConvertError> {
    if !src.exists() {
        return Ok(());
    }
    say(format!("Moving {what}…"));
    if dst_conflicts(dst, opts, plan_src) {
        if !opts.overwrite {
            return Err(ConvertError::Exists(dst.display().to_string()));
        }
        if !opts.dry_run && dst.exists() {
            fs::remove_dir_all(dst)?;
        }
        report.actions.push(ConvertAction {
            describe: format!("Remove existing {} at {}", what, dst.display()),
            from: None,
            to: Some(dst.to_path_buf()),
        });
    }
    report.actions.push(ConvertAction {
        describe: format!("Move {} {} -> {}", what, src.display(), dst.display()),
        from: Some(src.to_path_buf()),
        to: Some(dst.to_path_buf()),
    });
    if !opts.dry_run {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        // Prefer rename (same filesystem); fall back to copy+delete.
        if fs::rename(src, dst).is_err() {
            copy_recursive(src, dst)?;
            fs::remove_dir_all(src)?;
        }
    }
    Ok(())
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_recursive(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

/// Legacy layouts -> modern unified dimensions/ tree (for 26.1+ targets).
/// Moves top-level DIM content into `dimensions/`, relocates player files
/// into `players/`, and places a top-level seed file when the legacy
/// level.dat carries full worldgen settings. Returns true when anything was
/// planned (also true for dry runs).
#[allow(clippy::too_many_arguments)]
fn normalize_to_modern(
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
    src_root: &Path,
    remaps: &[(String, String)],
) -> Result<bool, ConvertError> {
    let root = &opts.server_root;
    let level = root.join(&opts.level_name);
    let src_level = src_root.join(&opts.level_name);

    if opts.in_place {
        maybe_backup(&level, opts, report, say)?;
    }
    let mut did_anything = false;

    // 1. Top-level DIM content -> dimensions/ tree (both legacy-split tops
    //    and vanilla-legacy DIM folders land here).
    let dim_sources = [
        (src_level.join("DIM-1"), "the_nether"),
        (src_level.join("DIM1"), "the_end"),
        (
            src_root
                .join(format!("{}_nether", opts.level_name))
                .join("DIM-1"),
            "the_nether",
        ),
        (
            src_root
                .join(format!("{}_the_end", opts.level_name))
                .join("DIM1"),
            "the_end",
        ),
    ];
    for (src_dim, dim) in dim_sources {
        if !src_dim.is_dir() {
            continue;
        }
        for store in ["region", "entities", "poi"] {
            let dst = level
                .join("dimensions")
                .join("minecraft")
                .join(dim)
                .join(store);
            // Skip no-op self-moves when source already lives there
            // (possible when re-running against a half-done tree).
            if same_file_location(&src_dim.join(store), &dst) {
                continue;
            }
            if merge_store_dir(
                &src_dim.join(store),
                &dst,
                opts,
                report,
                say,
                store,
                src_root,
            )? {
                did_anything = true;
            }
        }
    }

    // 2. Legacy playerdata/stats/advancements -> players/ tree.
    for (from, to) in [
        ("playerdata", "players/data"),
        ("stats", "players/stats"),
        ("advancements", "players/advancements"),
    ] {
        let src_store = src_level.join(from);
        let dst_store = level.join(to);
        if same_file_location(&src_store, &dst_store) {
            continue;
        }
        if merge_store_dir(&src_store, &dst_store, opts, report, say, from, src_root)? {
            did_anything = true;
        }
    }

    // 3. Seed file from a legacy level.dat carrying full worldgen settings.
    //    Ancient RandomSeed-only files can't produce dimension configs, so
    //    those warn instead of writing a file the game might choke on.
    let top_wgs = level
        .join("data")
        .join("minecraft")
        .join("world_gen_settings.dat");
    if !top_wgs.exists() {
        match crate::level::extract_seed_bundle(&src_level.join("level.dat")) {
            Ok(Some(bundle)) => {
                say("Placing world seed file…".to_string());
                report.actions.push(ConvertAction {
                    describe: format!("Write seed file {}", top_wgs.display()),
                    from: None,
                    to: Some(top_wgs.clone()),
                });
                if !opts.dry_run {
                    crate::level::write_seed_file(&top_wgs, &bundle, false)?;
                }
                did_anything = true;
            }
            Ok(None) => {
                report.warnings.push(
                    "No seed found to carry over (level.dat has no worldgen settings); new chunks will generate with a fresh seed.".into(),
                );
            }
            Err(e) => {
                report.warnings.push(format!(
                    "Could not read world seed from level.dat ({e}); new chunks may use a fresh seed."
                ));
            }
        }
    }

    // 4. Player remaps land in the modern players/ tree.
    for (from_uuid, to_uuid) in remaps {
        if remap_player_files(
            opts, report, say, &level, &src_level, from_uuid, to_uuid, true,
        )? {
            did_anything = true;
        }
    }

    if !did_anything {
        report
            .warnings
            .push("Nothing to normalize; the world already uses the modern unified layout.".into());
    }
    Ok(did_anything)
}

/// Minimal pass for sources already in the modern unified layout (Paper
/// 26.1+, vanilla 26.1+): place well-known dat files and the seed file where
/// vanilla expects them, remap players. Terrain never moves.
fn minimal_modern(
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
    src_root: &Path,
    remaps: &[(String, String)],
) -> Result<bool, ConvertError> {
    let root = &opts.server_root;
    let level = root.join(&opts.level_name);
    let src_level = src_root.join(&opts.level_name);
    let mut did_anything = false;

    // 1. Well-known overworld dat files belong at top-level data/minecraft/;
    //    chunk_tickets/raids/world_border stay per-dimension.
    let src_dat = src_level
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("data")
        .join("minecraft");
    let dst_dat = level.join("data").join("minecraft");
    if src_dat.is_dir() {
        let mut files: Vec<_> = std::fs::read_dir(&src_dat)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|n| {
                            crate::detect::PAPER_MODERN_DAT_FILES.contains(&n)
                                || n == "world_gen_settings.dat"
                        })
                        .unwrap_or(false)
            })
            .collect();
        files.sort();
        for src in files {
            let name = src
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let dst = dst_dat.join(&name);
            // Skip when source and destination are already the same file
            // (in-place runs where vanilla already placed it).
            if same_file_location(&src, &dst) {
                continue;
            }
            if dst_conflicts(&dst, opts, src_root) {
                if !opts.overwrite {
                    return Err(ConvertError::Exists(dst.display().to_string()));
                }
                report.actions.push(ConvertAction {
                    describe: format!("Replace existing {}", dst.display()),
                    from: None,
                    to: Some(dst.clone()),
                });
                if !opts.dry_run && dst.exists() {
                    fs::remove_file(&dst)?;
                }
            }
            say(format!("Placing {name}…"));
            report.actions.push(ConvertAction {
                describe: format!("Move dat {} -> {}", src.display(), dst.display()),
                from: Some(src.clone()),
                to: Some(dst.clone()),
            });
            if !opts.dry_run {
                fs::create_dir_all(&dst_dat)?;
                if fs::rename(&src, &dst).is_err() {
                    fs::copy(&src, &dst)?;
                    fs::remove_file(&src)?;
                }
            }
            did_anything = true;
        }
    }

    // 2. Seed file at top level, falling back through the dimensions.
    let top_wgs = dst_dat.join("world_gen_settings.dat");
    if !top_wgs.exists() {
        let dims_base = src_level.join("dimensions").join("minecraft");
        let mut placed = false;
        for dim in ["overworld", "the_nether", "the_end"] {
            let cand = dims_base
                .join(dim)
                .join("data")
                .join("minecraft")
                .join("world_gen_settings.dat");
            if cand.is_file() && !same_file_location(&cand, &top_wgs) {
                say("Placing world seed file…".to_string());
                report.actions.push(ConvertAction {
                    describe: format!("Copy seed file {} -> {}", cand.display(), top_wgs.display()),
                    from: Some(cand.clone()),
                    to: Some(top_wgs.clone()),
                });
                if !opts.dry_run {
                    if let Some(parent) = top_wgs.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(&cand, &top_wgs)?;
                }
                placed = true;
                did_anything = true;
                break;
            }
        }
        if !placed {
            report.warnings.push(
                "No seed file found in dimensions/; vanilla generates fresh worldgen for new chunks.".into(),
            );
        }
    }

    // 3. Player remaps inside the modern players/ tree.
    for (from_uuid, to_uuid) in remaps {
        if remap_player_files(
            opts, report, say, &level, &src_level, from_uuid, to_uuid, true,
        )? {
            did_anything = true;
        }
    }

    if !did_anything {
        report
            .warnings
            .push("Already in the modern unified layout; nothing to move.".into());
    }
    Ok(did_anything)
}

/// True when both paths point at the same filesystem location (guards
/// self-moves on re-runs and in-place layouts).
fn same_file_location(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Legacy (`world_nether/DIM-1`, `world_the_end/DIM1`) and
/// Paper-modern dat files -> Vanilla layout.
/// Returns true when anything was planned (also true for dry runs).
/// `src_root` is where sources are read from; in dry-run copy mode the
/// output dir does not exist yet, so sources come from the original root
/// while destinations point at the planned output.
fn convert_to_vanilla(
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
    src_root: &Path,
    remaps: &[(String, String)],
) -> Result<bool, ConvertError> {
    let root = &opts.server_root;
    let level = root.join(&opts.level_name);
    let nether = root.join(format!("{}_nether", opts.level_name));
    let end = root.join(format!("{}_the_end", opts.level_name));
    let src_level = src_root.join(&opts.level_name);
    let src_nether = src_root.join(format!("{}_nether", opts.level_name));
    let src_end = src_root.join(format!("{}_the_end", opts.level_name));

    // Back up anything we will mutate.
    for d in [&level, &nether, &end] {
        if d.is_dir() {
            maybe_backup(d, opts, report, say)?;
        }
    }

    let mut did_anything = false;

    // 1. Legacy split folders -> world/DIM-1, world/DIM1
    let moves = [
        (
            src_nether.join("DIM-1"),
            level.join("DIM-1"),
            "Nether DIM-1",
        ),
        (src_end.join("DIM1"), level.join("DIM1"), "End DIM1"),
    ];
    for (src, dst, what) in moves {
        if src.is_dir() {
            move_or_copy_dir(&src, &dst, opts, report, what, say, src_root)?;
            did_anything = true;
        }
    }

    // 2. Paper modern dimensions/* content -> Vanilla layout.
    //    overworld/{region,entities,poi} -> ./{region,entities,poi}
    //    overworld/data/minecraft/*     -> data/minecraft/*
    //    the_nether/{...}               -> DIM-1/{...} (+ data)
    //    the_end/{...}                  -> DIM1/{...} (+ data)
    //    Paper-only files (paper-world.yml, data/paper/) stay where they
    //    are; vanilla ignores them.
    let src_dims = src_root
        .join(&opts.level_name)
        .join("dimensions")
        .join("minecraft");
    let dim_targets = [
        ("overworld", level.clone()),
        ("the_nether", level.join("DIM-1")),
        ("the_end", level.join("DIM1")),
    ];
    let mut did_terrain = false;
    for (dim, vanilla_dir) in dim_targets {
        let src_base = src_dims.join(dim);
        if !src_base.is_dir() {
            continue;
        }
        for store in ["region", "entities", "poi"] {
            let src_store = src_base.join(store);
            let dst_store = vanilla_dir.join(store);
            if same_file_location(&src_store, &dst_store) {
                continue;
            }
            if merge_store_dir(&src_store, &dst_store, opts, report, say, store, src_root)? {
                did_anything = true;
                did_terrain = true;
            }
        }
        // All namespaced dat files (not just the 5 well-known ones).
        let src_dat = src_base.join("data").join("minecraft");
        let dst_dat = vanilla_dir.join("data").join("minecraft");
        if src_dat.is_dir() {
            let mut files: Vec<_> = std::fs::read_dir(&src_dat)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            files.sort();
            for src in files {
                let Some(name) = src.file_name().map(|s| s.to_string_lossy().to_string()) else {
                    continue;
                };
                let dst = dst_dat.join(&name);
                // Already placed (e.g. re-running in place): nothing to do.
                if same_file_location(&src, &dst) {
                    continue;
                }
                if dst_conflicts(&dst, opts, src_root) {
                    if !opts.overwrite {
                        return Err(ConvertError::Exists(dst.display().to_string()));
                    }
                    report.actions.push(ConvertAction {
                        describe: format!("Replace existing {}", dst.display()),
                        from: None,
                        to: Some(dst.clone()),
                    });
                    if !opts.dry_run && dst.exists() {
                        fs::remove_file(&dst)?;
                    }
                }
                report.actions.push(ConvertAction {
                    describe: format!("Move dat {} -> {}", src.display(), dst.display()),
                    from: Some(src.clone()),
                    to: Some(dst.clone()),
                });
                if !opts.dry_run {
                    fs::create_dir_all(&dst_dat)?;
                    if fs::rename(&src, &dst).is_err() {
                        fs::copy(&src, &dst)?;
                        fs::remove_file(&src)?;
                    }
                }
                did_anything = true;
                did_terrain = true;
            }
        }
    }
    if did_terrain {
        // Paper-only remnants are harmless to vanilla; say so explicitly
        // instead of leaving the user wondering about the leftover tree.
        report.warnings.push(
            "Paper-only files stay under dimensions/ (paper-world.yml, data/paper) — vanilla ignores them; safe to keep or delete.".into(),
        );
    }

    // 3. Seed: Paper keeps it in world_gen_settings.dat, vanilla needs it in
    //    level.dat — without this vanilla generates a fresh seed (different
    //    world, relocated spawn).
    {
        let work_wgs = level
            .join("data")
            .join("minecraft")
            .join("world_gen_settings.dat");
        let src_wgs = src_root
            .join(&opts.level_name)
            .join("dimensions")
            .join("minecraft")
            .join("overworld")
            .join("data")
            .join("minecraft")
            .join("world_gen_settings.dat");
        let wgs = if work_wgs.is_file() {
            work_wgs
        } else {
            src_wgs
        };
        let work_level = level.join("level.dat");
        let lvl = if work_level.is_file() {
            work_level
        } else {
            src_level.join("level.dat")
        };
        match crate::level::ensure_world_gen_settings(&lvl, &wgs, opts.dry_run) {
            Ok(crate::level::SeedInjection::Injected { seed }) => {
                say(format!("Injecting world seed {seed} into level.dat…"));
                let dst_level = level.join("level.dat");
                report.actions.push(ConvertAction {
                    describe: format!("Inject world seed {seed} into {}", dst_level.display()),
                    from: Some(wgs.clone()),
                    to: Some(dst_level),
                });
                did_anything = true;
            }
            Ok(_) => {}
            Err(e) => {
                report.warnings.push(format!(
                    "Could not check world seed in level.dat ({e}); vanilla may generate a fresh seed."
                ));
            }
        }
    }

    // 4. Paper player storage players/{data,stats,advancements} ->
    //    vanilla playerdata/, stats/, advancements/.
    let player_maps = [
        ("data", "playerdata"),
        ("stats", "stats"),
        ("advancements", "advancements"),
    ];
    let src_players = src_level.join("players");
    if src_players.is_dir() {
        for (from, to) in player_maps {
            if merge_store_dir(
                &src_players.join(from),
                &level.join(to),
                opts,
                report,
                say,
                "player data",
                src_root,
            )? {
                did_anything = true;
            }
        }
    }

    // Offline -> online UUID remaps, pre-resolved by the caller (manual pair
    // plus auto-discovered usercache pairs): offline-mode servers store
    // players under v3 UUIDs while online accounts use v4, so without this
    // the game spawns a fresh empty player. Legacy Anvil layout here.
    for (from_uuid, to_uuid) in remaps {
        if remap_player_files(
            opts, report, say, &level, &src_level, from_uuid, to_uuid, false,
        )? {
            did_anything = true;
        }
    }

    if !did_anything {
        // Distinguish "already vanilla" from unknown (check the source side,
        // which in dry-run copy mode is the only side that exists yet).
        let looks_vanilla = src_level.join("level.dat").is_file();
        if looks_vanilla {
            report
                .warnings
                .push("World already looks Vanilla-like; nothing to move.".into());
        } else {
            return Err(ConvertError::NothingToDo(
                "No legacy DIM folders or Paper-modern dat files found.".into(),
            ));
        }
    }

    // Cleanup empty legacy tops (only when we moved them and they are now empty-ish).
    for top in [&nether, &end] {
        if top.is_dir() && !opts.dry_run {
            let is_empty = fs::read_dir(top)
                .map(|mut it| it.next().is_none())
                .unwrap_or(false);
            // Also treat tops containing only empty DIM dirs as removable.
            if is_empty {
                fs::remove_dir(top)?;
                report.actions.push(ConvertAction {
                    describe: format!("Remove empty {}", top.display()),
                    from: None,
                    to: Some(top.clone()),
                });
            }
        }
    }

    Ok(did_anything)
}

/// Vanilla single-folder -> legacy split layout (for old Spigot targets).
/// Returns true when anything was planned (also true for dry runs).
fn convert_to_legacy(
    opts: &ConvertOptions,
    report: &mut ConvertReport,
    say: &mut dyn FnMut(String),
    src_root: &Path,
) -> Result<bool, ConvertError> {
    let root = &opts.server_root;
    let level = root.join(&opts.level_name);
    let nether = root.join(format!("{}_nether", opts.level_name));
    let end = root.join(format!("{}_the_end", opts.level_name));
    let src_level = src_root.join(&opts.level_name);

    maybe_backup(&level, opts, report, say)?;

    let moves = [
        (
            src_level.join("DIM-1"),
            nether.join("DIM-1"),
            "Nether DIM-1",
        ),
        (src_level.join("DIM1"), end.join("DIM1"), "End DIM1"),
    ];
    let mut did_anything = false;
    for (src, dst, what) in moves {
        if src.is_dir() {
            if !opts.dry_run {
                if let Some(p) = dst.parent() {
                    fs::create_dir_all(p)?;
                }
            }
            move_or_copy_dir(&src, &dst, opts, report, what, say, src_root)?;
            did_anything = true;
        }
    }
    if !did_anything {
        report
            .warnings
            .push("No Vanilla DIM-1/DIM1 folders found; nothing to split.".into());
    }
    let _ = Layout::LegacySplit;
    Ok(did_anything)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::detect_layout;
    use std::fs;

    #[test]
    fn legacy_to_vanilla_dry_run() {
        let root = std::env::temp_dir().join(format!("mcsc-conv-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world_nether").join("DIM-1").join("region")).unwrap();
        fs::create_dir_all(root.join("srv/world_the_end").join("DIM1").join("region")).unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: true,
            overwrite: false,
            progress: None,
            in_place: true,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.actions.iter().any(|a| a.describe.contains("DIM-1")));
        // dry run must not have moved
        assert!(root.join("srv/world_nether").join("DIM-1").is_dir());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_to_vanilla_real() {
        let root = std::env::temp_dir().join(format!("mcsc-conv2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world_nether").join("DIM-1")).unwrap();
        fs::write(
            root.join("srv/world_nether")
                .join("DIM-1")
                .join("marker.txt"),
            b"nether",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: true,
            progress: None,
            in_place: true,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        convert(&opts).unwrap();
        assert!(root
            .join("srv/world")
            .join("DIM-1")
            .join("marker.txt")
            .is_file());
        let d = detect_layout(&root.join("srv"), "world");
        assert_eq!(d.layout, crate::detect::Layout::Vanilla);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_mode_leaves_source_untouched() {
        let root = std::env::temp_dir().join(format!("mcsc-conv3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world_nether").join("DIM-1")).unwrap();
        fs::write(
            root.join("srv/world_nether")
                .join("DIM-1")
                .join("marker.txt"),
            b"nether",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        assert!(rep.source_untouched);
        let out = rep.output_root.clone().unwrap();
        // Converted copy has merged DIM; source still split.
        assert!(out.join("world").join("DIM-1").join("marker.txt").is_file());
        assert!(root.join("srv/world_nether").join("DIM-1").is_dir());
        assert!(!root.join("srv/world").join("DIM-1").exists());
        // No backup needed: source was never touched.
        assert!(rep.backups.is_empty());
        let d = detect_layout(&out, "world");
        assert_eq!(d.layout, crate::detect::Layout::Vanilla);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_mode_dry_run_writes_nothing() {
        let root = std::env::temp_dir().join(format!("mcsc-conv4-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world_nether").join("DIM-1")).unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: true,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(!rep.applied);
        assert!(rep.source_untouched);
        let out = rep.output_root.clone().unwrap();
        assert!(!out.exists());
        assert!(rep.actions.iter().any(|a| a.describe.starts_with("Copy")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn papermodern_terrain_moves_to_vanilla() {
        let root = std::env::temp_dir().join(format!("mcsc-conv5-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("srv/world/dimensions/minecraft");
        fs::create_dir_all(base.join("overworld/region")).unwrap();
        fs::write(base.join("overworld/region/r.0.0.mca"), b"chunk").unwrap();
        fs::create_dir_all(base.join("overworld/entities")).unwrap();
        fs::create_dir_all(base.join("the_nether/region")).unwrap();
        fs::write(base.join("the_nether/region/r.0.0.mca"), b"nether").unwrap();
        fs::create_dir_all(base.join("overworld/data/minecraft")).unwrap();
        fs::write(base.join("overworld/data/minecraft/weather.dat"), b"w").unwrap();
        // Paper-only leftovers must survive (and not count as content).
        fs::write(base.join("overworld/paper-world.yml"), b"p").unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        // Detection sees terrain under dimensions/ with Bukkit markers.
        let d = crate::detect::detect_layout(&root.join("srv"), "world");
        assert_eq!(d.layout, crate::detect::Layout::PaperModern);

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        assert!(rep.source_untouched);
        let out = rep.output_root.clone().unwrap();
        // Terrain merged into vanilla locations in the copy.
        // (Empty store dirs are skipped; the server recreates them.)
        assert!(out.join("world/region/r.0.0.mca").is_file());
        assert!(!out.join("world/entities").exists());
        assert!(out.join("world/DIM-1/region/r.0.0.mca").is_file());
        assert!(out.join("world/data/minecraft/weather.dat").is_file());
        // Source still has everything.
        assert!(root
            .join("srv/world/dimensions/minecraft/overworld/region/r.0.0.mca")
            .is_file());
        // Result re-detects as vanilla (leftovers don't count).
        let d2 = crate::detect::detect_layout(&out, "world");
        assert_eq!(d2.layout, crate::detect::Layout::Vanilla);
        let _ = fs::remove_dir_all(&root);
    }

    /// Half-converted source: the same dat file exists both at the vanilla
    /// location and under dimensions/ (exactly what an interrupted or
    /// dat-only convert leaves behind).
    fn half_converted_fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mcsc-conv-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("srv/world/dimensions/minecraft");
        fs::create_dir_all(base.join("overworld/region")).unwrap();
        fs::write(base.join("overworld/region/r.0.0.mca"), b"chunk").unwrap();
        fs::create_dir_all(base.join("overworld/data/minecraft")).unwrap();
        fs::write(base.join("overworld/data/minecraft/weather.dat"), b"new").unwrap();
        fs::create_dir_all(root.join("srv/world/data/minecraft")).unwrap();
        fs::write(root.join("srv/world/data/minecraft/weather.dat"), b"stale").unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();
        root
    }

    #[test]
    fn dry_run_predicts_post_copy_collisions() {
        let root = half_converted_fixture("half-dry");
        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: true,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        // The copy would place a stale weather.dat where the move wants to
        // go. Copy mode replaces stale files inside the fresh output, so the
        // preview must show the replacement instead of failing only mid-run.
        let rep = convert(&opts).unwrap();
        assert!(!rep.applied);
        assert!(
            rep.actions
                .iter()
                .any(|a| a.describe.contains("Replace existing")
                    && a.describe.contains("weather.dat")),
            "preview should flag the stale-file replacement"
        );
        let out = rep.output_root.clone().unwrap();
        assert!(!out.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_mode_replaces_stale_files_inside_fresh_copy() {
        let root = half_converted_fixture("half-real");
        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        // No overwrite flag needed: the output is a fresh copy, and the
        // source of truth is the layout being converted from.
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        let out = rep.output_root.clone().unwrap();
        assert_eq!(
            fs::read(out.join("world/data/minecraft/weather.dat")).unwrap(),
            b"new"
        );
        // Source untouched on both sides.
        assert_eq!(
            fs::read(root.join("srv/world/data/minecraft/weather.dat")).unwrap(),
            b"stale"
        );
        assert!(root
            .join("srv/world/dimensions/minecraft/overworld/data/minecraft/weather.dat")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn papermodern_players_map_to_vanilla() {
        let root = std::env::temp_dir().join(format!("mcsc-conv6-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("srv/world/dimensions/minecraft");
        fs::create_dir_all(base.join("overworld/region")).unwrap();
        fs::write(base.join("overworld/region/r.0.0.mca"), b"chunk").unwrap();
        fs::create_dir_all(root.join("srv/world/players/data")).unwrap();
        fs::write(root.join("srv/world/players/data/abc.dat"), b"p").unwrap();
        fs::create_dir_all(root.join("srv/world/players/stats")).unwrap();
        fs::write(root.join("srv/world/players/stats/abc.json"), b"{}").unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        let out = rep.output_root.clone().unwrap();
        assert!(out.join("world/playerdata/abc.dat").is_file());
        assert!(out.join("world/stats/abc.json").is_file());
        // Source untouched.
        assert!(root.join("srv/world/players/data/abc.dat").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn uuid_remap_copies_player_files() {
        let root = std::env::temp_dir().join(format!("mcsc-conv7-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world/playerdata")).unwrap();
        fs::write(
            root.join("srv/world/playerdata/aaaaaaaa-0000-4000-8000-000000000001.dat"),
            b"pdata",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world/stats")).unwrap();
        fs::write(
            root.join("srv/world/stats/aaaaaaaa-0000-4000-8000-000000000001.json"),
            b"{}",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: Some((
                "aaaaaaaa-0000-4000-8000-000000000001".into(),
                "bbbbbbbb-0000-4000-8000-000000000002".into(),
            )),
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        assert!(rep
            .actions
            .iter()
            .any(|a| a.describe.contains("Remap player")));
        let out = rep.output_root.clone().unwrap();
        assert_eq!(
            fs::read(out.join("world/playerdata/bbbbbbbb-0000-4000-8000-000000000002.dat"))
                .unwrap(),
            b"pdata"
        );
        assert!(out
            .join("world/stats/bbbbbbbb-0000-4000-8000-000000000002.json")
            .is_file());
        // Offline originals kept as fallback, in output and source.
        assert!(out
            .join("world/playerdata/aaaaaaaa-0000-4000-8000-000000000001.dat")
            .is_file());
        assert!(root
            .join("srv/world/playerdata/aaaaaaaa-0000-4000-8000-000000000001.dat")
            .is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn uuid_remap_dry_run_plans_only() {
        let root = std::env::temp_dir().join(format!("mcsc-conv8-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world/players/data")).unwrap();
        fs::write(
            root.join("srv/world/players/data/aaaaaaaa-0000-4000-8000-000000000001.dat"),
            b"pdata",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: true,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: Some((
                "aaaaaaaa-0000-4000-8000-000000000001".into(),
                "bbbbbbbb-0000-4000-8000-000000000002".into(),
            )),
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(!rep.applied);
        assert!(rep
            .actions
            .iter()
            .any(|a| a.describe.contains("Remap player")));
        let out = rep.output_root.clone().unwrap();
        assert!(!out.exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn convert_many_runs_each_world_and_collects_errors() {
        let root = std::env::temp_dir().join(format!("mcsc-convmany-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for w in ["w1", "w2"] {
            fs::create_dir_all(root.join("srv").join(w).join("DIM-1")).unwrap();
            fs::write(root.join("srv").join(w).join("level.dat"), b"x").unwrap();
        }
        let base = ConvertOptions {
            server_root: root.join("srv"),
            level_name: String::new(),
            target: TargetLayout::LegacySplit,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert_many(
            &base,
            &["w1".to_string(), "missing".to_string(), "w2".to_string()],
        );
        assert_eq!(rep.per_world.len(), 3);
        assert!(rep.per_world[0].1.is_ok());
        assert!(rep.per_world[1].1.is_err());
        assert!(rep.per_world[2].1.is_ok());
        assert_eq!(rep.succeeded(), 2);
        assert_eq!(rep.failed(), 1);
        assert!(root.join("w1-converted-legacy").is_dir());
        assert!(root.join("w2-converted-legacy").is_dir());
        // Sources untouched.
        assert!(root.join("srv/w1").join("DIM-1").is_dir());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn papermodern_injects_seed_into_output_level_dat() {
        let root = std::env::temp_dir().join(format!("mcsc-conv9-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("srv/world/dimensions/minecraft");
        fs::create_dir_all(base.join("overworld/region")).unwrap();
        fs::write(base.join("overworld/region/r.0.0.mca"), b"chunk").unwrap();
        fs::create_dir_all(base.join("overworld/data/minecraft")).unwrap();
        // Minimal level.dat without WorldGenSettings + seed carrier.
        let level: fastnbt::Value = fastnbt::Value::Compound(
            [(
                "Data".to_string(),
                fastnbt::Value::Compound(
                    [
                        ("DataVersion".to_string(), fastnbt::Value::Int(4903)),
                        (
                            "LevelName".to_string(),
                            fastnbt::Value::String("world".into()),
                        ),
                    ]
                    .into_iter()
                    .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        );
        let raw = fastnbt::to_bytes(&level).unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        let f = std::fs::File::create(root.join("srv/world/level.dat")).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        use std::io::Write;
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
        let wgs = fastnbt::Value::Compound(
            [(
                "data".to_string(),
                fastnbt::Value::Compound(
                    [("seed".to_string(), fastnbt::Value::Long(12345))]
                        .into_iter()
                        .collect(),
                ),
            )]
            .into_iter()
            .collect(),
        );
        let raw = fastnbt::to_bytes(&wgs).unwrap();
        let f = std::fs::File::create(base.join("overworld/data/minecraft/world_gen_settings.dat"))
            .unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();

        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        assert!(rep
            .actions
            .iter()
            .any(|a| a.describe.contains("Inject world seed 12345")));
        // Seed readable back from the OUTPUT level.dat the way vanilla reads it.
        let out_level = root.join("world-converted-vanilla/world/level.dat");
        let raw = {
            use std::io::Read;
            let mut buf = Vec::new();
            flate2::read::GzDecoder::new(std::fs::File::open(&out_level).unwrap())
                .read_to_end(&mut buf)
                .unwrap();
            buf
        };
        let v: fastnbt::Value = fastnbt::from_bytes(&raw).unwrap();
        let data = match &v {
            fastnbt::Value::Compound(m) => m.get("Data"),
            _ => None,
        };
        let seed = match data {
            Some(fastnbt::Value::Compound(m)) => match m.get("WorldGenSettings") {
                Some(fastnbt::Value::Compound(w)) => w.get("seed").cloned(),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(seed, Some(fastnbt::Value::Long(12345)));
        // Source level.dat untouched (no seed injected there).
        let src_raw = {
            use std::io::Read;
            let mut buf = Vec::new();
            flate2::read::GzDecoder::new(
                std::fs::File::open(root.join("srv/world/level.dat")).unwrap(),
            )
            .read_to_end(&mut buf)
            .unwrap();
            buf
        };
        let sv: fastnbt::Value = fastnbt::from_bytes(&src_raw).unwrap();
        let has_wgs = match &sv {
            fastnbt::Value::Compound(m) => match m.get("Data") {
                Some(fastnbt::Value::Compound(d)) => d.contains_key("WorldGenSettings"),
                _ => false,
            },
            _ => false,
        };
        assert!(!has_wgs);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_output_dir_lands_outside_server() {
        let root = std::env::temp_dir().join(format!("mcsc-convout-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world/level.dat"), b"x").unwrap();
        fs::create_dir_all(root.join("srv/world_nether/DIM-1")).unwrap();
        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: Some(root.join("elsewhere")),
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        let out = rep.output_root.clone().unwrap();
        assert_eq!(out, root.join("elsewhere/world-converted-vanilla"));
        assert!(out.join("world/DIM-1").is_dir());
        // Nothing extra next to the source.
        assert!(!root.join("world-converted-vanilla").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_remap_uses_usercache_and_stub_lookup() {
        use crate::players::UuidResolver;
        use std::sync::{Arc, Mutex};
        let root = std::env::temp_dir().join(format!("mcsc-convauto-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // Paper-style source with two offline players.
        for name in ["Anivia31", "GhostNoFile"] {
            let off = crate::players::offline_uuid(name);
            fs::create_dir_all(root.join("srv/world/players/data")).unwrap();
            fs::write(
                root.join("srv/world/players/data")
                    .join(format!("{off}.dat")),
                b"pdata",
            )
            .unwrap();
        }
        // usercache knows both plus a stranger with no files.
        fs::write(
            root.join("srv/usercache.json"),
            r#"[{"name":"Anivia31","uuid":"5ad08d99-26a2-331c-8f77-d476dec25826"},{"name":"GhostNoFile","uuid":"00000000-0000-3000-8000-000000000000"},{"name":"Stranger","uuid":"11111111-1111-4111-8111-111111111111"}]"#,
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = calls.clone();
        let resolver = UuidResolver::new(move |name: &str| {
            seen.lock().unwrap().push(name.to_string());
            match name {
                // Only Anivia31 resolves; GhostNoFile is unknown online.
                "Anivia31" => Some("10b8a89c-bb59-4113-a62f-5ac96b915dd9".to_string()),
                _ => None,
            }
        });
        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::Vanilla,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: true,
            uuid_resolver: Some(resolver),
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        let out = rep.output_root.clone().unwrap();
        // Remapped copy exists with identical bytes; offline original kept.
        assert_eq!(
            fs::read(out.join("world/playerdata/10b8a89c-bb59-4113-a62f-5ac96b915dd9.dat"))
                .unwrap(),
            b"pdata"
        );
        assert!(out
            .join("world/playerdata/5ad08d99-26a2-331c-8f77-d476dec25826.dat")
            .is_file());
        // Lookups only ran for names that actually have files (Stranger skipped).
        let mut got = calls.lock().unwrap().clone();
        got.sort();
        assert_eq!(got, vec!["Anivia31".to_string(), "GhostNoFile".to_string()]);
        assert!(rep
            .actions
            .iter()
            .any(|a| a.describe.contains("Remap player")));
        let _ = fs::remove_dir_all(&root);
    }

    fn write_gz_nbt(path: &std::path::Path, value: &fastnbt::Value) {
        let raw = fastnbt::to_bytes(value).unwrap();
        let f = std::fs::File::create(path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        use std::io::Write;
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
    }

    fn base_opts(root: &std::path::Path, target: TargetLayout) -> ConvertOptions {
        ConvertOptions {
            server_root: root.to_path_buf(),
            level_name: "world".into(),
            target,
            backup: false,
            dry_run: false,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        }
    }

    #[test]
    fn legacy_split_normalizes_to_modern() {
        let root = std::env::temp_dir().join(format!("mcsc-convnorm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // Legacy split world with a 1.20.5+ level.dat carrying WorldGenSettings.
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::create_dir_all(root.join("srv/world_nether/DIM-1/region")).unwrap();
        fs::write(root.join("srv/world_nether/DIM-1/region/r.0.0.mca"), b"n").unwrap();
        fs::create_dir_all(root.join("srv/world/playerdata")).unwrap();
        fs::write(root.join("srv/world/playerdata/abc.dat"), b"p").unwrap();
        let mut wgs = std::collections::HashMap::new();
        wgs.insert("seed".to_string(), fastnbt::Value::Long(777));
        let mut data = std::collections::HashMap::new();
        data.insert("DataVersion".to_string(), fastnbt::Value::Int(4903));
        data.insert(
            "WorldGenSettings".to_string(),
            fastnbt::Value::Compound(wgs),
        );
        let mut r = std::collections::HashMap::new();
        r.insert("Data".to_string(), fastnbt::Value::Compound(data));
        write_gz_nbt(
            &root.join("srv/world/level.dat"),
            &fastnbt::Value::Compound(r),
        );

        let opts = base_opts(&root.join("srv"), TargetLayout::VanillaModern);
        let rep = convert(&opts).unwrap();
        assert!(rep.applied, "normalize should plan moves");
        assert!(rep.source_untouched);
        let out = rep.output_root.clone().unwrap();
        // Nether terrain merged into the dimensions/ tree.
        assert!(out
            .join("world/dimensions/minecraft/the_nether/region/r.0.0.mca")
            .is_file());
        assert!(!out.join("world/DIM-1").exists());
        // Players relocated, seed file placed.
        assert!(out.join("world/players/data/abc.dat").is_file());
        assert!(out
            .join("world/data/minecraft/world_gen_settings.dat")
            .is_file());
        // Source intact.
        assert!(root
            .join("srv/world_nether/DIM-1/region/r.0.0.mca")
            .is_file());
        assert!(root.join("srv/world/playerdata/abc.dat").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn modern_minimal_places_dat_and_seed() {
        let root = std::env::temp_dir().join(format!("mcsc-convmin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("srv/world/dimensions/minecraft");
        fs::create_dir_all(base.join("overworld/region")).unwrap();
        fs::write(base.join("overworld/region/r.0.0.mca"), b"c").unwrap();
        fs::create_dir_all(base.join("overworld/data/minecraft")).unwrap();
        fs::write(base.join("overworld/data/minecraft/game_rules.dat"), b"g").unwrap();
        fs::create_dir_all(base.join("the_nether/data/minecraft")).unwrap();
        fs::write(
            base.join("the_nether/data/minecraft/world_gen_settings.dat"),
            b"seed-src-marker",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = base_opts(&root.join("srv"), TargetLayout::VanillaModern);
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        let out = rep.output_root.clone().unwrap();
        // Famous dat moved to top; seed file taken from nether fallback.
        assert!(out.join("world/data/minecraft/game_rules.dat").is_file());
        assert!(out
            .join("world/data/minecraft/world_gen_settings.dat")
            .is_file());
        // Terrain NOT duplicated to legacy locations.
        assert!(!out.join("world/region").exists());
        assert!(!out.join("world/DIM-1").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn modern_already_placed_is_noop() {
        let root = std::env::temp_dir().join(format!("mcsc-convnoop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("srv/world/dimensions/minecraft");
        fs::create_dir_all(base.join("overworld/region")).unwrap();
        fs::write(base.join("overworld/region/r.0.0.mca"), b"c").unwrap();
        // Marker file so the overworld dat dir exists but holds nothing to place.
        fs::create_dir_all(base.join("overworld/data/minecraft")).unwrap();
        fs::write(base.join("overworld/data/minecraft/raids.dat"), b"r").unwrap();
        fs::create_dir_all(root.join("srv/world/data/minecraft")).unwrap();
        fs::write(
            root.join("srv/world/data/minecraft/world_gen_settings.dat"),
            b"s",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();

        let opts = base_opts(&root.join("srv"), TargetLayout::VanillaModern);
        let rep = convert(&opts).unwrap();
        assert!(!rep.applied);
        assert!(rep.warnings.iter().any(|w| w.contains("nothing to move")));
        let _ = fs::remove_dir_all(&root);
    }

    fn auto_fixture(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("mcsc-convauto-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn auto_pairs_local_v3_v4_without_network() {
        use crate::players::UuidResolver;
        let root = auto_fixture("local");
        // Offline file named with the TRUE derived uuid for "Somebody".
        let off = crate::players::offline_uuid("Somebody");
        assert_eq!(&off[14..15], "3");
        fs::create_dir_all(root.join("srv/world/players/data")).unwrap();
        fs::write(
            root.join("srv/world/players/data")
                .join(format!("{off}.dat")),
            b"p",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();
        fs::write(
            root.join("srv/usercache.json"),
            format!(r#"[{{"name":"Somebody","uuid":"{off}"}}]"#),
        )
        .unwrap();
        fs::write(
            root.join("srv/ops.json"),
            r#"[{"uuid":"bbbbbbbb-0000-4000-8000-000000000002","name":"Somebody","level":4}]"#,
        )
        .unwrap();
        // Resolver must never fire: the pair is certain locally. Panic if called.
        let resolver = UuidResolver::new(|_: &str| -> Option<String> {
            panic!("network lookup must not run for local pairs")
        });
        let mut opts = base_opts(&root.join("srv"), TargetLayout::Vanilla);
        opts.auto_remap_all = true;
        opts.uuid_resolver = Some(resolver);
        let rep = convert(&opts).unwrap();
        assert!(rep.applied);
        let out = rep.output_root.clone().unwrap();
        assert_eq!(
            fs::read(out.join("world/playerdata/bbbbbbbb-0000-4000-8000-000000000002.dat"))
                .unwrap(),
            b"p"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn auto_names_unmapped_files_instead_of_dropping() {
        let root = auto_fixture("unmapped");
        fs::create_dir_all(root.join("srv/world/players/data")).unwrap();
        fs::write(
            root.join("srv/world/players/data/cccccccc-0000-3000-8000-000000000003.dat"),
            b"p",
        )
        .unwrap();
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world").join("level.dat"), b"x").unwrap();
        // usercache knows only someone else.
        fs::write(
            root.join("srv/usercache.json"),
            r#"[{"name":"Nobody","uuid":"dddddddd-0000-3000-8000-000000000004"}]"#,
        )
        .unwrap();
        let mut opts = base_opts(&root.join("srv"), TargetLayout::Vanilla);
        opts.auto_remap_all = true;
        opts.uuid_resolver = None;
        let rep = convert(&opts).unwrap();
        assert!(
            rep.warnings
                .iter()
                .any(|w| w.contains("cccccccc-0000-3000-8000-000000000003")),
            "unmapped file must be named, got: {:?}",
            rep.warnings
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn default_output_sits_next_to_server_folder() {
        let root = std::env::temp_dir().join(format!("mcsc-convsib-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world/level.dat"), b"x").unwrap();
        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::VanillaModern,
            backup: false,
            dry_run: true,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: None,
        };
        let rep = convert(&opts).unwrap();
        let out = rep.output_root.clone().unwrap();
        // Next to the server folder, never inside it (or inside the world).
        assert_eq!(out.parent().unwrap(), root);
        assert!(!out.starts_with(root.join("srv")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn output_inside_world_is_refused() {
        let root = std::env::temp_dir().join(format!("mcsc-convin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("srv/world")).unwrap();
        fs::write(root.join("srv/world/level.dat"), b"x").unwrap();
        let opts = ConvertOptions {
            server_root: root.join("srv"),
            level_name: "world".into(),
            target: TargetLayout::VanillaModern,
            backup: false,
            dry_run: true,
            overwrite: false,
            progress: None,
            in_place: false,
            uuid_remap: None,
            auto_remap_all: false,
            uuid_resolver: None,
            output_dir: Some(root.join("srv/world")),
        };
        let err = convert(&opts).unwrap_err().to_string();
        assert!(
            err.contains("inside the world folder"),
            "must refuse with guidance, got: {err}"
        );
        // Nothing was created anywhere.
        assert!(!root.join("world-converted-vanilla26").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
