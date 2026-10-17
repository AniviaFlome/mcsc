use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::progress::ProgressReporter;
use crate::region::read_chunks;

#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    pub server_root: PathBuf,
    pub level_name: String,
    /// Max .mca files to open (0 = unlimited).
    pub max_files: usize,
    /// Optional progress reports (per region file). Called from the worker
    /// thread; keep it cheap and non-blocking.
    pub progress: Option<ProgressReporter>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub files_scanned: usize,
    pub chunks_scanned: usize,
    pub chunks_with_modded: usize,
    /// namespace -> occurrences (e.g. "biomesoplenty" -> 1234)
    pub namespaces: BTreeMap<String, u64>,
    /// full id -> occurrences for top offenders (capped)
    pub top_ids: Vec<(String, u64)>,
    pub warnings: Vec<String>,
    pub modded: bool,
}

pub fn scan_modded(opts: &ScanOptions) -> ScanReport {
    let mut rep = ScanReport::default();
    let level = opts.server_root.join(&opts.level_name);
    let mut region_dirs = vec![
        level.join("region"),
        level.join("DIM-1").join("region"),
        level.join("DIM1").join("region"),
        // legacy split mirrors (scanned if user points at server root with level_name=world)
        opts.server_root
            .join(format!("{}_nether", opts.level_name))
            .join("DIM-1")
            .join("region"),
        opts.server_root
            .join(format!("{}_the_end", opts.level_name))
            .join("DIM1")
            .join("region"),
        // paper modern dimension paths
        level
            .join("dimensions")
            .join("minecraft")
            .join("overworld")
            .join("region"),
    ];
    // Also catch any nested region/ dirs (modded dimensions, e.g. DIM7).
    let mut extra = Vec::new();
    if level.is_dir() {
        for entry in WalkDir::new(&level).into_iter().filter_map(|e| e.ok()) {
            if entry.file_type().is_dir() && entry.file_name() == "region" {
                extra.push(entry.path().to_path_buf());
            }
        }
    }
    region_dirs.extend(extra);
    region_dirs.sort();
    region_dirs.dedup();

    let mut id_counts: HashMap<String, u64> = HashMap::new();
    let mut files = 0usize;

    // Collect files first so progress can report a total.
    let mut all_mcas: Vec<PathBuf> = Vec::new();
    for dir in &region_dirs {
        if !dir.is_dir() {
            continue;
        }
        let mut mcas: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|e| e == "mca").unwrap_or(false))
                .collect(),
            Err(_) => continue,
        };
        mcas.sort();
        all_mcas.extend(mcas);
    }
    let total_files = all_mcas.len() as u64;
    if let Some(p) = &opts.progress {
        p.report(0, total_files, "Scanning region files…");
    }

    for mca in all_mcas {
        if opts.max_files > 0 && files >= opts.max_files {
            rep.warnings
                .push(format!("Stopped after {} files (cap).", opts.max_files));
            break;
        }
        files += 1;
        rep.files_scanned += 1;
        if let Some(p) = &opts.progress {
            let name = mca
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            p.report(files as u64, total_files, format!("Scanning {name}…"));
        }
        let chunks = match read_chunks(&mca) {
            Ok(c) => c,
            Err(e) => {
                rep.warnings.push(format!("{}: {e}", mca.display()));
                continue;
            }
        };
        for chunk in chunks {
            rep.chunks_scanned += 1;
            let ids = extract_ids(&chunk.bytes);
            if !ids.is_empty() {
                rep.chunks_with_modded += 1;
                for id in ids {
                    *id_counts.entry(id.clone()).or_default() += 1;
                    if let Some((ns, _)) = id.split_once(':') {
                        *rep.namespaces.entry(ns.to_string()).or_default() += 1;
                    }
                }
            }
        }
    }
    if let Some(p) = &opts.progress {
        p.report(total_files, total_files, "Scan complete.");
    }

    // Vanilla namespaces are not modded; drop them.
    for vanilla in ["minecraft", "mojang", "brigadier"] {
        rep.namespaces.remove(vanilla);
    }
    let mut ids: Vec<(String, u64)> = id_counts
        .into_iter()
        .filter(|(k, _)| {
            !k.starts_with("minecraft:")
                && !k.starts_with("mojang:")
                && !k.starts_with("brigadier:")
        })
        .collect();
    ids.sort_by_key(|a| std::cmp::Reverse(a.1));
    ids.truncate(50);
    rep.top_ids = ids;
    rep.modded = !rep.namespaces.is_empty();
    if !rep.modded && rep.chunks_scanned > 0 {
        rep.warnings
            .push("No non-vanilla block/entity IDs found; world looks vanilla-safe.".into());
    }
    rep
}

/// Best-effort modded ID extraction:
/// 1. Try fastnbt generic parse and walk strings.
/// 2. Fall back to raw byte scan for `namespace:value` patterns.
fn extract_ids(nbt_bytes: &[u8]) -> Vec<String> {
    if let Ok(v) = fastnbt::from_bytes::<fastnbt::Value>(nbt_bytes) {
        let mut acc = Vec::new();
        walk_value(&v, &mut acc);
        // Deduplicate per chunk but keep counts in caller (one entry per chunk per id set is enough).
        acc.sort();
        acc.dedup();
        // Keep only namespaced ids that are not vanilla.
        return acc
            .into_iter()
            .filter(|s| {
                s.contains(':')
                    && !s.starts_with("minecraft:")
                    && !s.starts_with("mojang:")
                    && !s.starts_with("brigadier:")
                    && is_plausible_id(s)
            })
            .collect();
    }
    byte_scan_ids(nbt_bytes)
}

fn walk_value(v: &fastnbt::Value, acc: &mut Vec<String>) {
    match v {
        fastnbt::Value::String(s) => {
            if s.contains(':') {
                acc.push(s.clone());
            }
        }
        fastnbt::Value::Compound(m) => {
            for value in m.values() {
                walk_value(value, acc);
            }
        }
        fastnbt::Value::List(l) => {
            for value in l {
                walk_value(value, acc);
            }
        }
        _ => {}
    }
}

fn is_plausible_id(s: &str) -> bool {
    let Some((ns, path)) = s.split_once(':') else {
        return false;
    };
    if ns.is_empty() || path.is_empty() || ns.len() > 64 || path.len() > 128 {
        return false;
    }
    let ok = |c: char| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/')
    };
    ns.chars().all(ok) && path.chars().all(ok)
}

fn byte_scan_ids(bytes: &[u8]) -> Vec<String> {
    // Scan printable runs for namespace:path tokens.
    let mut out = Vec::new();
    let mut cur = Vec::new();
    let push = |cur: &mut Vec<u8>, out: &mut Vec<String>| {
        if cur.len() >= 4 {
            if let Ok(s) = std::str::from_utf8(cur) {
                if s.contains(':') && is_plausible_id(s) && !s.starts_with("minecraft:") {
                    out.push(s.to_string());
                }
            }
        }
        cur.clear();
    };
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/' | b':') {
            cur.push(b);
            if cur.len() > 200 {
                push(&mut cur, &mut out);
            }
        } else {
            push(&mut cur, &mut out);
        }
    }
    push(&mut cur, &mut out);
    out.sort();
    out.dedup();
    out
}

/// Write a human-readable report file next to the world (caller decides path).
pub fn write_report_text(rep: &ScanReport, dest: &Path) -> std::io::Result<()> {
    use std::fmt::Write as _;
    let mut s = String::new();
    writeln!(s, "mcsc modded-content scan (report-only)").unwrap();
    writeln!(
        s,
        "files: {} chunks: {} with-modded: {}",
        rep.files_scanned, rep.chunks_scanned, rep.chunks_with_modded
    )
    .unwrap();
    if rep.namespaces.is_empty() {
        writeln!(s, "result: no non-vanilla namespaces found").unwrap();
    } else {
        writeln!(s, "namespaces:").unwrap();
        for (ns, n) in &rep.namespaces {
            writeln!(s, "  {ns}: {n}").unwrap();
        }
        writeln!(s, "top ids:").unwrap();
        for (id, n) in &rep.top_ids {
            writeln!(s, "  {id}: {n}").unwrap();
        }
        writeln!(s, "\nNote: these blocks/items/entities will vanish or break on Vanilla/Paper without the original mods.").unwrap();
    }
    for w in &rep.warnings {
        writeln!(s, "warn: {w}").unwrap();
    }
    std::fs::write(dest, s)
}
