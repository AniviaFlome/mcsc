use std::path::PathBuf;

use mcsc_core::{ConvertManyReport, ConvertOptions, ProgressReporter, ScanOptions, UuidResolver};

use crate::state::{do_detect, lookup_online_uuid, resolve_remap, App, Banner, BannerKind};

fn progress_reporter(handle: rune::AppHandle<App>) -> ProgressReporter {
    ProgressReporter::new(move |p| {
        let p2 = p.clone();
        handle.update(move |s: &mut App, _| {
            s.progress = Some(p2);
        });
    })
}

pub fn start_scan(s: &mut App) {
    if s.is_busy() {
        s.push("Still working — wait for the current task to finish.");
        return;
    }
    let (root, level, note) = s.resolve_dirs();
    if root.as_os_str().is_empty() {
        s.push("Pick a server folder first (Browse or type the path), then Scan.");
        return;
    }
    if let Some(n) = note {
        s.push(&n);
    }
    let Some(handle) = s.handle.clone() else {
        s.push("UI handle not ready — try again.");
        return;
    };
    eprintln!("[mcsc] message: Scan");
    s.busy = Some("Scanning region files…".into());
    s.progress = None;
    s.banner = Some(Banner {
        kind: BannerKind::Working,
        text: "Scan started…".into(),
    });
    let reporter = progress_reporter(handle.clone());
    let opts = ScanOptions {
        server_root: root,
        level_name: level,
        max_files: 0,
        progress: Some(reporter),
    };
    std::thread::spawn(move || {
        let rep = mcsc_core::scan_modded(&opts);
        handle.update(move |s: &mut App, _| {
            eprintln!("[mcsc] message: ScanFinished");
            s.busy = None;
            s.progress = None;
            let summary = format!(
                "Scan complete: {} files, {} chunks, {} with modded IDs.",
                rep.files_scanned, rep.chunks_scanned, rep.chunks_with_modded
            );
            s.push(&summary);
            s.banner = Some(Banner {
                kind: BannerKind::Ok,
                text: summary,
            });
            s.scan = Some(rep);
        });
    });
}

pub fn start_convert(s: &mut App) {
    if s.is_busy() {
        s.push("Still working — wait for the current task to finish.");
        return;
    }
    let (root, level, note) = s.resolve_dirs();
    if root.as_os_str().is_empty() {
        s.push("Pick a server folder first (Browse or type the path), then Convert.");
        return;
    }
    if let Some(n) = note {
        s.push(&n);
    }
    let uuid_remap = match resolve_remap(s) {
        Ok(r) => r,
        Err(e) => {
            s.push(&e);
            return;
        }
    };
    let Some(handle) = s.handle.clone() else {
        s.push("UI handle not ready — try again.");
        return;
    };
    eprintln!("[mcsc] message: Convert");
    let reporter = progress_reporter(handle.clone());
    let opts = ConvertOptions {
        server_root: root,
        level_name: level,
        target: s.target.to_core(),
        backup: s.backup,
        dry_run: s.dry_run,
        overwrite: s.overwrite,
        progress: Some(reporter),
        in_place: s.in_place,
        uuid_remap,
        output_dir: s.output_base(),
        auto_remap_all: s.auto_remap,
        uuid_resolver: s.auto_remap.then(|| UuidResolver::new(lookup_online_uuid)),
    };
    let mode = if opts.dry_run { "dry run" } else { "REAL" };
    let dry = opts.dry_run;
    let target_label = s.target.label().to_string();
    // Convert-all: same options for every detected world set.
    let worlds: Option<Vec<String>> = if s.convert_all {
        s.detection
            .as_ref()
            .map(|d| d.worlds.iter().map(|w| w.level_name.clone()).collect())
    } else {
        None
    };
    match worlds {
        Some(list) if !list.is_empty() => {
            s.busy = Some(format!("Converting {} worlds ({mode})…", list.len()));
            s.progress = None;
            s.banner = Some(Banner {
                kind: BannerKind::Working,
                text: format!("Convert started for {} worlds ({mode})…", list.len()),
            });
            std::thread::spawn(move || {
                let rep = mcsc_core::convert_many(&opts, &list);
                handle.update(move |s: &mut App, _| {
                    finish_many(s, rep, dry);
                });
            });
        }
        _ => {
            s.busy = Some(format!("Converting ({mode})…"));
            s.progress = None;
            s.banner = Some(Banner {
                kind: BannerKind::Working,
                text: format!("Convert started ({mode})…"),
            });
            std::thread::spawn(move || {
                let res = mcsc_core::convert(&opts).map_err(|e| e.to_string());
                handle.update(move |s: &mut App, _| {
                    finish_single(s, res, dry, &target_label);
                });
            });
        }
    }
}

fn finish_single(
    s: &mut App,
    res: Result<mcsc_core::ConvertReport, String>,
    dry_run: bool,
    target_label: &str,
) {
    eprintln!("[mcsc] message: ConvertFinished");
    s.busy = None;
    s.progress = None;
    match res {
        Ok(rep) => {
            let summary = if dry_run {
                format!(
                    "Dry run complete: {} steps previewed — no files changed.",
                    rep.actions.len()
                )
            } else if !rep.applied {
                "Convert finished with nothing to apply.".to_string()
            } else if rep.source_untouched {
                match &rep.output_root {
                    Some(out) => format!(
                        "Converted to {target_label} in a new copy: {} — source untouched.",
                        out.display()
                    ),
                    None => format!("Converted to {target_label} — source untouched."),
                }
            } else {
                format!(
                    "Converted source in place: {} steps applied, {} backup(s).",
                    rep.actions.len(),
                    rep.backups.len()
                )
            };
            s.push(&summary);
            for a in &rep.actions {
                s.push(&format!("  {}", a.describe));
            }
            s.banner = Some(Banner {
                kind: BannerKind::Ok,
                text: summary.clone(),
            });
            s.convert = Some(rep.clone());
            s.convert_err = None;
            // Re-verify the result so the UI shows the difference.
            if rep.applied {
                let (root, level, _) = s.resolve_dirs();
                let check_root: PathBuf = rep.output_root.clone().unwrap_or_else(|| root.clone());
                let before = s.detection.as_ref().map(|d| d.layout.label().to_string());
                crate::state::detect_into(s, &check_root, &level);
                // Keep the convert report visible (detect clears it).
                s.convert = Some(rep.clone());
                let now = s
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
                s.push(&done);
                s.banner = Some(Banner {
                    kind: BannerKind::Ok,
                    text: done,
                });
            }
        }
        Err(e) => {
            s.push(&format!("Convert failed: {e}"));
            s.banner = Some(Banner {
                kind: BannerKind::Err,
                text: format!("Convert failed: {e}"),
            });
            s.convert = None;
            s.convert_err = Some(e);
        }
    }
}

fn finish_many(s: &mut App, rep: ConvertManyReport, dry_run: bool) {
    eprintln!("[mcsc] message: ConvertManyFinished");
    s.busy = None;
    s.progress = None;
    let ok = rep.succeeded();
    let failed = rep.failed();
    let total = rep.per_world.len();
    let headline = if dry_run {
        format!("Dry run complete for {total} world(s) — no files changed.")
    } else if failed == 0 {
        format!("Converted {ok}/{total} world(s).")
    } else {
        format!("Converted {ok}/{total} world(s), {failed} failed.")
    };
    s.push(&headline);
    for (level, res) in &rep.per_world {
        match res {
            Ok(r) => {
                let steps = r.actions.len();
                let where_ = match &r.output_root {
                    Some(o) => format!("new copy: {}", o.display()),
                    None => "in place".to_string(),
                };
                s.push(&format!(
                    "  [{level}] {} ({} steps, {})",
                    if r.applied { "done" } else { "no changes" },
                    steps,
                    where_
                ));
            }
            Err(e) => s.push(&format!("  [{level}] FAILED: {e}")),
        }
    }
    s.many = Some(rep);
    s.convert = None;
    s.convert_err = None;
    // Refresh the primary world's STATUS card (this resets the
    // banner, so restate the many-worlds summary afterwards).
    do_detect(s);
    s.banner = Some(Banner {
        kind: if failed == 0 {
            BannerKind::Ok
        } else {
            BannerKind::Err
        },
        text: headline,
    });
}
