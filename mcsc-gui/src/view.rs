use rune::{
    col, color::hex, el, row, spacer, text, Color, Cx, El, FileDialogOptions, ScrollSpec, Spring,
    TextStyle, Tx,
};
use rune_kit::{Button, ButtonVariant, Checkbox, Kit, PaletteSeed, Progress, TextInput};

use crate::state::{App, BannerKind, HintKind, Target};
use crate::workers::{start_convert, start_scan};

fn catppuccin(dark: bool) -> (Color, Color, Color) {
    // (bg, fg, accent) — Mocha / Latte.
    if dark {
        (
            hex("#1e1e2e").unwrap(),
            hex("#cdd6f4").unwrap(),
            hex("#cba6f7").unwrap(),
        )
    } else {
        (
            hex("#eff1f5").unwrap(),
            hex("#4c4f69").unwrap(),
            hex("#8839ef").unwrap(),
        )
    }
}

pub fn kit_for(dark: bool) -> Kit {
    let (bg, fg, accent) = catppuccin(dark);
    Kit::from_palette(PaletteSeed { bg, fg, accent })
}

fn mauve_fill() -> Color {
    hex("#8839ef").unwrap()
}

fn muted(dark: bool) -> Color {
    hex(if dark { "#bac2de" } else { "#6c6f85" }).unwrap()
}

fn ok_green(dark: bool) -> Color {
    hex(if dark { "#a6e3a1" } else { "#40a02b" }).unwrap()
}

fn err_red(dark: bool) -> Color {
    hex(if dark { "#f38ba8" } else { "#d20f39" }).unwrap()
}

fn warn_peach(dark: bool) -> Color {
    hex(if dark { "#fab387" } else { "#fe6409" }).unwrap()
}

fn section_title(s: &str, k: &Kit) -> El {
    text(s.to_string())
        .font(TextStyle::new(13.0, 700.0))
        .color(k.c.text_accent)
}

fn card(title: &str, k: &Kit, content: El) -> El {
    col()
        .padding(18.0)
        .gap(10.0)
        .fill(k.c.bg_base)
        .border(1.0, k.c.border_base)
        .radius(12.0)
        .child(section_title(title, k))
        .child(content)
}

fn field_row(label: &str, k: &Kit, input: El) -> El {
    col()
        .gap(4.0)
        .child(
            text(label.to_string())
                .font(TextStyle::new(14.0, 400.0))
                .color(k.c.text_base),
        )
        .child(input)
}

fn target_button(selected: Target, option: Target, _k: &Kit, dark: bool) -> El {
    let active = selected == option;
    let fg = Color::WHITE;
    let inner = col().gap(2.0).w_full().shrink().child(
        text(option.label().to_string())
            .font(TextStyle::new(15.0, 400.0))
            .color(if active {
                fg
            } else {
                hex(if dark { "#cdd6f4" } else { "#4c4f69" }).unwrap()
            })
            .wrap()
            .w_full(),
    );
    let mauve = mauve_fill();
    let mut btn = el()
        .w_full()
        .padding(12.0)
        .radius(10.0)
        .child(inner)
        .role("radio")
        .label(option.label().to_string())
        .focusable(true)
        // Selection swaps fill/border/text colors; land them at once instead
        // of Rune's default color-spring fade.
        .motion(Spring::NONE);
    if active {
        let opt = option;
        btn = btn
            .fill(mauve)
            .border(2.0, mauve)
            .hover(mauve)
            .on_press(move |s: &mut App| {
                eprintln!("[mcsc] message: TargetSelected");
                s.target = opt;
            });
    } else {
        let opt = option;
        let (bg, fg2, accent) = catppuccin(dark);
        let _ = (bg, fg2, accent);
        btn = btn
            .fill(hex(if dark { "#313244" } else { "#e6e9ef" }).unwrap())
            .border(1.0, hex(if dark { "#45475a" } else { "#ccd0da" }).unwrap())
            .hover(hex(if dark { "#45475a" } else { "#ccd0da" }).unwrap())
            .on_press(move |s: &mut App| {
                eprintln!("[mcsc] message: TargetSelected");
                s.target = opt;
            });
    }
    btn.key(format!("target-{}", option.tag()))
}

fn banner_bar(s: &App, k: &Kit) -> Option<El> {
    let b = s.banner.as_ref()?;
    let (border, mark) = match b.kind {
        BannerKind::Info => (k.c.text_accent, "ⓘ"),
        BannerKind::Working => (k.c.text_accent, "…"),
        BannerKind::Ok => (ok_green(s.dark), "✓"),
        BannerKind::Err => (err_red(s.dark), "✗"),
    };
    let mut line = format!("{mark} {}", b.text);
    if b.kind == BannerKind::Working {
        if let Some(p) = &s.progress {
            if p.total > 0 {
                line.push_str(&format!(" — {} {}/{}", p.label, p.done, p.total));
            } else if !p.label.is_empty() {
                line.push_str(&format!(" — {}", p.label));
            }
        }
    }
    Some(
        el().w_full()
            .padding(12.0)
            .radius(10.0)
            .fill(k.c.bg_base)
            .border(1.0, border)
            .child(
                text(line)
                    .font(TextStyle::new(15.0, 400.0))
                    .color(k.c.text_base),
            )
            .key("banner"),
    )
}

fn status_card(s: &App, k: &Kit) -> El {
    let body: El = match (&s.detection, &s.level_info, &s.level_err) {
        (None, _, _) => text("No world detected yet.".to_string())
            .font(TextStyle::new(15.0, 400.0))
            .color(k.c.text_base),
        (Some(det), info, err) => {
            let mut c = col().gap(4.0);
            c = c.child(kv("Layout", det.layout.label().to_string(), k));
            c = c.child(kv(
                "World",
                format!("{}/{}", det.server_root.display(), det.level_name),
                k,
            ));
            if let Some(i) = info {
                c = c.child(kv(
                    "level.dat",
                    format!("DataVersion={:?} Name={:?}", i.data_version, i.level_name),
                    k,
                ));
            }
            if let Some(e) = err {
                c = c.child(
                    text(format!("level.dat: {e}"))
                        .font(TextStyle::new(14.0, 400.0))
                        .color(err_red(s.dark)),
                );
            }
            for n in &det.notes {
                c = c.child(
                    text(format!("ⓘ {n}"))
                        .font(TextStyle::new(13.0, 400.0))
                        .color(warn_peach(s.dark)),
                );
            }
            if !det.worlds.is_empty() {
                c = c.child(section_title("WORLDS IN FOLDER", k));
                for w in &det.worlds {
                    let mut line = format!("• {} ({})", w.level_name, w.layout.label());
                    if !w.mirrors.is_empty() {
                        line.push_str(&format!(" + {}", w.mirrors.join(", ")));
                    }
                    c = c.child(
                        text(line)
                            .font(TextStyle::new(13.0, 400.0))
                            .color(k.c.text_base),
                    );
                }
            }
            c
        }
    };
    card("STATUS", k, body).key("status-card")
}

fn kv(k_: &str, v: String, k: &Kit) -> El {
    row()
        .gap(8.0)
        .child(
            text(k_.to_string())
                .font(TextStyle::new(14.0, 400.0))
                .color(muted(k.dark))
                .w(130.0),
        )
        .child(
            text(v)
                .font(TextStyle::new(14.0, 400.0))
                .color(k.c.text_base),
        )
}

fn scan_card(s: &App, k: &Kit) -> Option<El> {
    let rep = s.scan.as_ref()?;
    let mut c = col().gap(6.0).child(
        text(format!(
            "{} files · {} chunks · {} with modded IDs",
            rep.files_scanned, rep.chunks_scanned, rep.chunks_with_modded
        ))
        .font(TextStyle::new(14.0, 400.0))
        .color(k.c.text_base),
    );
    if rep.namespaces.is_empty() {
        c = c.child(
            text("✓ No non-vanilla namespaces — vanilla-safe.".to_string())
                .font(TextStyle::new(14.0, 400.0))
                .color(k.c.text_base),
        );
    } else {
        for (ns, n) in rep.namespaces.iter().take(12) {
            c = c.child(
                row()
                    .child(
                        text(ns.clone())
                            .font(TextStyle::new(14.0, 400.0))
                            .color(k.c.text_base)
                            .grow(1.0),
                    )
                    .child(
                        text(format!("{n}"))
                            .font(TextStyle::new(14.0, 400.0))
                            .color(k.c.text_base),
                    ),
            );
        }
        if !rep.top_ids.is_empty() {
            c = c.child(section_title("TOP IDS", k));
            for (id, n) in rep.top_ids.iter().take(10) {
                c = c.child(
                    row()
                        .child(
                            text(id.clone())
                                .font(TextStyle::new(13.0, 400.0))
                                .color(k.c.text_base)
                                .grow(1.0),
                        )
                        .child(
                            text(format!("{n}"))
                                .font(TextStyle::new(13.0, 400.0))
                                .color(k.c.text_base),
                        ),
                );
            }
        }
    }
    for w in &rep.warnings {
        c = c.child(
            text(w.clone())
                .font(TextStyle::new(13.0, 400.0))
                .color(muted(s.dark)),
        );
    }
    Some(card("MOD SCAN (REPORT ONLY)", k, c).key("scan-card"))
}

fn convert_card(s: &App, k: &Kit) -> Option<El> {
    let body: El = if let Some(many) = &s.many {
        let mut c = col().gap(4.0);
        let mut shown = 0usize;
        for (level, res) in &many.per_world {
            match res {
                Ok(rep) => {
                    let status = if rep.applied { "done" } else { "no changes" };
                    c = c.child(
                        text(format!("—— {level}: {status} ——"))
                            .font(TextStyle::new(14.0, 400.0))
                            .color(k.c.text_base),
                    );
                    for a in rep.actions.iter().take(8) {
                        if shown >= 40 {
                            break;
                        }
                        c = c.child(
                            text(format!("• {}", a.describe))
                                .font(TextStyle::new(13.0, 400.0))
                                .color(k.c.text_base),
                        );
                        shown += 1;
                    }
                }
                Err(e) => {
                    c = c.child(
                        text(format!("—— {level}: FAILED: {e} ——"))
                            .font(TextStyle::new(14.0, 400.0))
                            .color(err_red(s.dark)),
                    );
                }
            }
        }
        c
    } else {
        match (&s.convert, &s.convert_err) {
            (None, None) => col().gap(4.0),
            (None, Some(e)) => col().gap(4.0).child(
                text(format!("Failed: {e}"))
                    .font(TextStyle::new(14.0, 400.0))
                    .color(err_red(s.dark)),
            ),
            (Some(rep), _) => {
                let mut c = col().gap(4.0);
                if rep.source_untouched {
                    match &rep.output_root {
                        Some(out) => {
                            c = c.child(
                                text(format!("⧉ Output (new copy): {}", out.display()))
                                    .font(TextStyle::new(14.0, 400.0))
                                    .color(k.c.text_base),
                            );
                            c = c.child(
                                text("Source folder was not modified.".to_string())
                                    .font(TextStyle::new(13.0, 400.0))
                                    .color(ok_green(s.dark)),
                            );
                        }
                        None => {
                            c = c.child(
                                text("Source folder was not modified.".to_string())
                                    .font(TextStyle::new(13.0, 400.0))
                                    .color(ok_green(s.dark)),
                            );
                        }
                    }
                } else if let Some(out) = &rep.output_root {
                    c = c.child(
                        text(format!("⧉ Output: {}", out.display()))
                            .font(TextStyle::new(14.0, 400.0))
                            .color(k.c.text_base),
                    );
                }
                for a in rep.actions.iter().take(30) {
                    c = c.child(
                        text(format!("• {}", a.describe))
                            .font(TextStyle::new(13.0, 400.0))
                            .color(k.c.text_base),
                    );
                }
                if rep.actions.len() > 30 {
                    c = c.child(
                        text(format!("… +{} more", rep.actions.len() - 30))
                            .font(TextStyle::new(13.0, 400.0))
                            .color(muted(s.dark)),
                    );
                }
                for w in &rep.warnings {
                    c = c.child(
                        text(format!("⚠ {w}"))
                            .font(TextStyle::new(13.0, 400.0))
                            .color(warn_peach(s.dark)),
                    );
                }
                for b in &rep.backups {
                    c = c.child(
                        text(format!("⧉ {}", b.display()))
                            .font(TextStyle::new(13.0, 400.0))
                            .color(k.c.text_base),
                    );
                }
                c
            }
        }
    };
    if s.many.is_some() || s.convert.is_some() || s.convert_err.is_some() {
        Some(card("CONVERT", k, body).key("convert-card"))
    } else {
        None
    }
}

fn log_card(s: &App, k: &Kit) -> Option<El> {
    if s.log.is_empty() {
        return None;
    }
    Some(
        card(
            "LOG",
            k,
            text(s.log.clone())
                .font(TextStyle::new(12.0, 400.0).mono())
                .color(k.c.text_base),
        )
        .key("log-card"),
    )
}

pub fn view(s: &App, _cx: &mut Cx<App>) -> El {
    let k = kit_for(s.dark);
    let dark = s.dark;

    // Header
    let theme_label = if dark { "● Mocha" } else { "○ Latte" };
    let header = row()
        .gap(8.0)
        .child(
            col()
                .gap(2.0)
                .child(
                    text("Minecraft Server World Converter".to_string())
                        .font(TextStyle::new(24.0, 700.0))
                        .color(k.c.text_accent),
                )
                .child(
                    text("Purpur · Paper · Spigot · Fabric → Vanilla, and back".to_string())
                        .font(TextStyle::new(13.0, 400.0))
                        .color(muted(dark)),
                ),
        )
        .child(spacer())
        .child(
            Button::new(theme_label)
                .variant(ButtonVariant::Neutral)
                .on_press_tx(|s: &mut App, tx: &mut Tx| {
                    eprintln!("[mcsc] message: ToggleTheme");
                    s.dark = !s.dark;
                    tx.color_motion(rune::tok::motion::CALM);
                })
                .build(&k)
                .key("theme"),
        );

    // Left cards
    let worlds_label = match s.detection.as_ref() {
        Some(d) if d.worlds.len() > 1 => {
            format!("Convert all {} worlds in folder", d.worlds.len())
        }
        _ => "Convert all worlds in folder".to_string(),
    };

    let server_input = TextInput::new(&s.server_root)
        .placeholder("Pick server folder…")
        .width(Some(240.0))
        .on_edit(|s: &mut App| &mut s.server_root)
        .on_change(|s: &mut App, _| {
            s.detection = None;
            s.refresh_output_default();
        })
        .build(&k)
        .key("server-root");
    let level_input = TextInput::new(&s.level_name)
        .placeholder("world")
        .width(Some(180.0))
        .on_edit(|s: &mut App| &mut s.level_name)
        .on_change(|s: &mut App, _| {
            s.detection = None;
        })
        .build(&k)
        .key("level-name");
    let browse_btn = Button::new("Browse")
        .variant(ButtonVariant::Neutral)
        .on_press_tx(|_: &mut App, tx: &mut Tx| {
            eprintln!("[mcsc] message: Browse");
            tx.open_folder(
                FileDialogOptions {
                    title: "Pick server folder".to_string(),
                    ..Default::default()
                },
                |s: &mut App, result, _| {
                    eprintln!("[mcsc] message: BrowseFinished");
                    match result {
                        Ok(Some(dir)) => {
                            s.server_root.text = dir.display().to_string();
                            s.refresh_output_default();
                            s.detection = None;
                            crate::state::do_detect(s);
                        }
                        Ok(None) => {
                            s.push("Browse cancelled.");
                        }
                        Err(e) => {
                            s.push(&format!("Browse failed: {}", e.0));
                        }
                    }
                },
            );
        })
        .build(&k)
        .key("browse");

    let source_card = card(
        "SOURCE",
        &k,
        col()
            .gap(10.0)
            .child(row().gap(8.0).child(server_input).child(browse_btn))
            .child(field_row("level-name", &k, level_input)),
    )
    .key("source-card");

    let output_input = TextInput::new(&s.output_dir)
        .placeholder("Next to server folder…")
        .width(Some(240.0))
        .on_edit(|s: &mut App| &mut s.output_dir)
        .on_change(|s: &mut App, _| {
            s.output_customized = true;
        })
        .build(&k)
        .key("output-dir");
    let browse_out_btn = Button::new("Browse")
        .variant(ButtonVariant::Neutral)
        .on_press_tx(|_: &mut App, tx: &mut Tx| {
            eprintln!("[mcsc] message: BrowseOutput");
            tx.open_folder(
                FileDialogOptions {
                    title: "Pick output folder".to_string(),
                    ..Default::default()
                },
                |s: &mut App, result, _| match result {
                    Ok(Some(dir)) => {
                        s.output_dir.text = dir.display().to_string();
                        s.output_customized = true;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        s.push(&format!("Browse output failed: {}", e.0));
                    }
                },
            );
        })
        .build(&k)
        .key("browse-output");
    let mut output_body = col()
        .gap(10.0)
        .child(row().gap(8.0).child(output_input).child(browse_out_btn));
    if !s.in_place {
        output_body = output_body.child(
            text(s.output_preview())
                .font(TextStyle::new(13.0, 400.0))
                .color(muted(dark)),
        );
    }
    let output_card = card("OUTPUT", &k, output_body).key("output-card");

    let mut target_col = col()
        .gap(10.0)
        .child(target_button(s.target, Target::VanillaModern, &k, dark))
        .child(target_button(s.target, Target::Vanilla, &k, dark))
        .child(target_button(s.target, Target::PaperModern, &k, dark))
        .child(target_button(s.target, Target::LegacySplit, &k, dark));
    {
        let label = worlds_label;
        target_col = target_col.child(
            Checkbox::new(s.convert_all)
                .label(label)
                .on_toggle(|s: &mut App| {
                    eprintln!("[mcsc] message: ConvertAllToggled");
                    s.convert_all = !s.convert_all;
                })
                .build(&k)
                .key("convert-all"),
        );
    }
    let target_card = card("TARGET", &k, target_col).key("target-card");

    let options_card = card(
        "OPTIONS",
        &k,
        col()
            .gap(10.0)
            .child(
                Checkbox::new(s.backup)
                    .label("Backup before converting")
                    .on_toggle(|s: &mut App| {
                        eprintln!("[mcsc] message: BackupToggled");
                        s.backup = !s.backup;
                    })
                    .build(&k)
                    .key("backup"),
            )
            .child(
                Checkbox::new(s.dry_run)
                    .label("Dry run")
                    .on_toggle(|s: &mut App| {
                        eprintln!("[mcsc] message: DryRunToggled");
                        s.dry_run = !s.dry_run;
                    })
                    .build(&k)
                    .key("dry-run"),
            )
            .child(
                Checkbox::new(s.overwrite)
                    .label("Overwrite destination")
                    .on_toggle(|s: &mut App| {
                        eprintln!("[mcsc] message: OverwriteToggled");
                        s.overwrite = !s.overwrite;
                    })
                    .build(&k)
                    .key("overwrite"),
            )
            .child(
                Checkbox::new(s.in_place)
                    .label("Convert in place")
                    .on_toggle(|s: &mut App| {
                        eprintln!("[mcsc] message: InPlaceToggled");
                        s.in_place = !s.in_place;
                    })
                    .build(&k)
                    .key("in-place"),
            ),
    )
    .key("options-card");

    let (hint_text, hint_kind) = s.player_hint();
    let hint_color = match hint_kind {
        HintKind::Ok => ok_green(dark),
        HintKind::Neutral => muted(dark),
        HintKind::Err => err_red(dark),
    };
    let player_card = card(
        "PLAYER",
        &k,
        col()
            .gap(10.0)
            .child(
                Checkbox::new(s.auto_remap)
                    .label("Auto-remap all players")
                    .on_toggle(|s: &mut App| {
                        eprintln!("[mcsc] message: AutoRemapToggled");
                        s.auto_remap = !s.auto_remap;
                    })
                    .build(&k)
                    .key("auto-remap"),
            )
            .child(
                TextInput::new(&s.player_name)
                    .placeholder("Offline username (on the server)")
                    .width(None)
                    .on_edit(|s: &mut App| &mut s.player_name)
                    .build(&k)
                    .key("player-name"),
            )
            .child(
                TextInput::new(&s.online_uuid)
                    .placeholder("Online UUID (your account now)")
                    .width(None)
                    .on_edit(|s: &mut App| &mut s.online_uuid)
                    .build(&k)
                    .key("online-uuid"),
            )
            .child(
                text(hint_text)
                    .font(TextStyle::new(13.0, 400.0))
                    .color(hint_color)
                    .key("player-hint"),
            ),
    )
    .key("player-card");

    let left_inner = col()
        .gap(12.0)
        .child(source_card)
        .child(output_card)
        .child(target_card)
        .child(player_card)
        .child(options_card);
    let left = left_inner
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .w(400.0)
        .h_full()
        .min_h(0.0)
        .key("left");

    // Right column
    let mut right_col = col().gap(12.0);
    right_col = right_col.child(status_card(s, &k));
    if let Some(sc) = scan_card(s, &k) {
        right_col = right_col.child(sc);
    }
    if let Some(cc) = convert_card(s, &k) {
        right_col = right_col.child(cc);
    }
    if let Some(lc) = log_card(s, &k) {
        right_col = right_col.child(lc);
    }
    // Progress indicator inside right column when busy
    if s.is_busy() {
        let prog_val = match &s.progress {
            Some(p) if p.total > 0 => Some(p.done as f32 / p.total as f32),
            _ => None,
        };
        let prog_label = match &s.progress {
            Some(p) if !p.label.is_empty() => p.label.clone(),
            _ => s.busy.clone().unwrap_or_default(),
        };
        right_col = right_col.child(
            card(
                "WORKING",
                &k,
                Progress::new(prog_val).label(prog_label).build(&k),
            )
            .key("working"),
        );
    }
    let right = right_col
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
        .h_full()
        .min_h(0.0)
        .key("right");

    let main_row = row()
        .gap(16.0)
        .h_full()
        .min_h(0.0)
        .grow(1.0)
        .child(left)
        .child(right);

    // Footer
    let busy_label: El = match (&s.busy, &s.progress) {
        (Some(_), Some(p)) if p.total > 0 => text(format!("{} {}/{}", p.label, p.done, p.total))
            .font(TextStyle::new(14.0, 400.0))
            .color(k.c.text_accent),
        (Some(_), Some(p)) => text(p.label.clone())
            .font(TextStyle::new(14.0, 400.0))
            .color(k.c.text_accent),
        (Some(b), None) => text(b.clone())
            .font(TextStyle::new(14.0, 400.0))
            .color(k.c.text_accent),
        (None, _) => text("".to_string()).font(TextStyle::new(13.0, 400.0)),
    };
    let convert_variant = if s.in_place && !s.dry_run {
        ButtonVariant::Danger
    } else {
        ButtonVariant::Contrast
    };
    let footer = row()
        .gap(10.0)
        .child(
            Button::new("Detect")
                .variant(ButtonVariant::Neutral)
                .on_press(|s: &mut App| {
                    eprintln!("[mcsc] message: Detect");
                    crate::state::do_detect(s);
                })
                .build(&k)
                .key("detect"),
        )
        .child(
            Button::new("Scan mods")
                .variant(ButtonVariant::Neutral)
                .on_press(|s: &mut App| {
                    start_scan(s);
                })
                .build(&k)
                .key("scan"),
        )
        .child(
            Button::new("Convert")
                .variant(convert_variant)
                .on_press(|s: &mut App| {
                    start_convert(s);
                })
                .build(&k)
                .key("convert"),
        )
        .child(
            Button::new("Clear")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut App| {
                    eprintln!("[mcsc] message: Clear");
                    s.scan = None;
                    s.convert = None;
                    s.convert_err = None;
                    s.many = None;
                    s.banner = None;
                    s.progress = None;
                    s.log.clear();
                })
                .build(&k)
                .key("clear"),
        )
        .child(spacer())
        .child(busy_label.key("busy-label"));

    let mut page = col()
        .gap(16.0)
        .padding(24.0)
        .w_full()
        .h_full()
        .fill(k.c.page)
        .child(header.key("header"));
    if let Some(bar) = banner_bar(s, &k) {
        page = page.child(bar);
    }
    page = page.child(main_row).child(footer.key("footer"));

    // Root must fill window; center like old view.
    col()
        .w_full()
        .h_full()
        .fill(k.c.page)
        .child(page)
        .key("root")
}
