use std::path::{Path, PathBuf};

/// On-disk world layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Layout {
    /// Vanilla / Fabric / Forge pre-26.1 (legacy Anvil):
    /// `<root>/<level>/`, `<root>/<level>/DIM-1`, `<root>/<level>/DIM1`,
    /// seed inside level.dat.
    Vanilla,
    /// Vanilla / Fabric / Forge 26.1+: same dimensions/ tree as Paper
    /// modern, no Bukkit markers. Seed in `data/minecraft/world_gen_settings.dat`.
    VanillaModern,
    /// Paper >= 26.1 and modern Purpur/Folia forks:
    /// terrain + entities + poi under
    /// `dimensions/minecraft/{overworld,the_nether,the_end}/{region,entities,poi}/`
    /// plus namespaced dat files under `dimensions/minecraft/*/data/minecraft/`.
    /// Paper-only leftovers (paper-world.yml, data/paper/) do not count.
    PaperModern,
    /// Spigot / CraftBukkit / Paper < 26.1:
    /// `<root>/<level>/`, `<root>/<level>_nether/DIM-1`, `<root>/<level>_the_end/DIM1`
    LegacySplit,
    /// Could not classify.
    Unknown,
}

impl Layout {
    pub fn label(self) -> &'static str {
        match self {
            Layout::Vanilla => "Vanilla pre-26.1 (legacy Anvil)",
            Layout::VanillaModern => "Vanilla 26.1+ (dimensions layout)",
            Layout::PaperModern => "Paper modern 26.1+ (Paper/Purpur/Folia)",
            Layout::LegacySplit => "Legacy split (Spigot/CraftBukkit/Paper pre-26.1)",
            Layout::Unknown => "Unknown",
        }
    }

    /// Modern unified storage (dimensions/ tree), whatever the branding.
    pub fn is_modern(self) -> bool {
        matches!(self, Layout::PaperModern | Layout::VanillaModern)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    pub layout: Layout,
    pub server_root: PathBuf,
    pub level_name: String,
    pub notes: Vec<String>,
    /// Every world set found in the server folder (Multiverse etc.).
    pub worlds: Vec<FoundWorld>,
}

/// One world set in a server folder.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundWorld {
    pub level_name: String,
    pub layout: Layout,
    /// Attached mirrors, e.g. `world_nether`, `world_the_end`.
    pub mirrors: Vec<String>,
}

/// Files Paper 26.1+ stores under dimensions/... that Vanilla expects under data/.
/// Kept for reference; conversion moves *all* files found in the modern
/// `data/minecraft` dirs, not just these.
pub const PAPER_MODERN_DAT_FILES: &[&str] = &[
    "game_rules.dat",
    "scheduled_events.dat",
    "wandering_trader.dat",
    "weather.dat",
    "world_gen_settings.dat",
];

/// Per-dimension storage subdirs that move between layouts
/// (region = terrain, entities, poi).
pub const DIMENSION_STORE_DIRS: &[&str] = &["region", "entities", "poi"];

/// Paper 26.1+ dimension ids under `dimensions/minecraft/`.
pub const PAPER_DIMENSIONS: &[&str] = &["overworld", "the_nether", "the_end"];

/// Read level-name from server.properties if present, else default.
pub fn read_level_name(server_root: &Path, default: &str) -> String {
    let props = server_root.join("server.properties");
    let Ok(text) = std::fs::read_to_string(&props) else {
        return default.to_string();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "level-name" {
                let v = v.trim();
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
    }
    default.to_string()
}

pub fn detect_layout(server_root: &Path, level_name: &str) -> Detection {
    let mut notes = Vec::new();
    let layout = classify_layout(server_root, level_name, &mut notes);
    let worlds = find_worlds(server_root);

    Detection {
        layout,
        server_root: server_root.to_path_buf(),
        level_name: level_name.to_string(),
        notes,
        worlds,
    }
}

/// True when the world dir carries Bukkit/Paper markers: paper-world.yml,
/// a data/paper dir (top level or per dimension), or a Bukkit.Version tag
/// in level.dat. Vanilla 26.1+ shares the dimensions/ layout but has none
/// of these.
fn has_bukkit_markers(level: &Path) -> bool {
    if level.join("paper-world.yml").is_file() || level.join("data").join("paper").is_dir() {
        return true;
    }
    let dims = level.join("dimensions").join("minecraft");
    for dim in ["overworld", "the_nether", "the_end"] {
        let base = dims.join(dim);
        if base.join("paper-world.yml").is_file() || base.join("data").join("paper").is_dir() {
            return true;
        }
    }
    // Fall back to the level.dat tag (cheap: small file, parsed anyway).
    let Ok(raw) = std::fs::read(level.join("level.dat")) else {
        return false;
    };
    let bytes = if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut dec = flate2::read::GzDecoder::new(&raw[..]);
        let mut out = Vec::new();
        use std::io::Read;
        if dec.read_to_end(&mut out).is_err() {
            return false;
        }
        out
    } else {
        raw
    };
    let Ok(value) = fastnbt::from_bytes::<fastnbt::Value>(&bytes) else {
        return false;
    };
    matches!(&value, fastnbt::Value::Compound(map) if map
        .get("Data")
        .is_some_and(|d| matches!(d, fastnbt::Value::Compound(m) if m.contains_key("Bukkit.Version"))))
}

/// Find every world set in a server folder: any top-level dir holding
/// level.dat, except legacy mirrors (`X_nether`/`X_the_end` attach to `X`
/// when it exists), hidden dirs, backups (`*.bak-*`) and previous outputs
/// (`*-converted-*`).
pub fn find_worlds(server_root: &Path) -> Vec<FoundWorld> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(server_root) {
        for entry in rd.filter_map(|e| e.ok()) {
            let Ok(ty) = entry.file_type() else {
                continue;
            };
            if !ty.is_dir() && !ty.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if name.contains("-converted-") || name.contains(".bak-") {
                continue;
            }
            if !entry.path().join("level.dat").is_file() {
                continue;
            }
            names.push(name);
        }
    }
    names.sort();
    let mut worlds = Vec::new();
    for name in &names {
        // Legacy mirrors belong to their base world, not to themselves —
        // unless the base is missing (orphan), in which case list it alone.
        let mut base: Option<&str> = None;
        for suffix in ["_nether", "_the_end"] {
            if let Some(b) = name.strip_suffix(suffix) {
                if names.iter().any(|n| n == b) {
                    base = Some(b);
                }
                break;
            }
        }
        if base.is_some() {
            continue;
        }
        let layout = classify_layout(server_root, name, &mut Vec::new());
        let mut mirrors = Vec::new();
        for suffix in ["_nether", "_the_end"] {
            let m = format!("{name}{suffix}");
            if server_root.join(&m).is_dir() {
                mirrors.push(m);
            }
        }
        worlds.push(FoundWorld {
            level_name: name.clone(),
            layout,
            mirrors,
        });
    }
    worlds
}

fn classify_layout(server_root: &Path, level_name: &str, notes: &mut Vec<String>) -> Layout {
    let level = server_root.join(level_name);
    let nether = server_root.join(format!("{level_name}_nether"));
    let end = server_root.join(format!("{level_name}_the_end"));

    let legacy_nether_dim = nether.join("DIM-1");
    let legacy_end_dim = end.join("DIM1");
    let vanilla_nether_dim = level.join("DIM-1");
    let vanilla_end_dim = level.join("DIM1");
    let paper_modern_data = level
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("data")
        .join("minecraft");

    let has_legacy = legacy_nether_dim.is_dir() || legacy_end_dim.is_dir();
    let has_vanilla_dims = vanilla_nether_dim.is_dir() || vanilla_end_dim.is_dir();
    // Paper-modern content: terrain (.mca) under dimensions/.../{region,entities,poi}
    // or namespaced dat files under dimensions/.../data/minecraft. Paper-only
    // leftovers (paper-world.yml, data/paper/) must NOT trigger this, so that
    // a converted world re-detects as Vanilla.
    let dims_base = level.join("dimensions").join("minecraft");
    let mut has_modern_terrain = false;
    let mut has_modern_dat = false;
    'scan: for dim in ["overworld", "the_nether", "the_end"] {
        let base = dims_base.join(dim);
        for store in ["region", "entities", "poi"] {
            let dir = base.join(store);
            if dir.is_dir()
                && std::fs::read_dir(&dir)
                    .map(|mut it| {
                        it.any(|e| {
                            e.map(|e| e.path().extension().map(|x| x == "mca").unwrap_or(false))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            {
                has_modern_terrain = true;
                break 'scan;
            }
        }
        let dat = base.join("data").join("minecraft");
        if dat.is_dir()
            && std::fs::read_dir(&dat)
                .map(|mut it| {
                    it.any(|e| {
                        e.map(|e| e.path().extension().map(|x| x == "dat").unwrap_or(false))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        {
            has_modern_dat = true;
        }
    }
    // Legacy constant kept for the historical dat-file check in tests.
    let has_legacy_dat_list = PAPER_MODERN_DAT_FILES
        .iter()
        .any(|f| paper_modern_data.join(f).is_file());
    let has_modern_dat = has_modern_dat || has_legacy_dat_list;

    // level.dat sanity
    if !level.join("level.dat").is_file() {
        notes.push(format!(
            "No {}/level.dat found; is '{}' the right server root / level-name?",
            level_name,
            server_root.display()
        ));
    }

    if has_legacy {
        if has_vanilla_dims {
            notes.push("Both legacy split folders and Vanilla DIM folders exist; treating as LegacySplit (clean up Vanilla DIM-1/DIM1 copies before converting).".into());
        }
        Layout::LegacySplit
    } else if has_modern_terrain || has_modern_dat {
        // Same dimensions/ tree for Paper and vanilla 26.1+; tell them apart
        // by Bukkit markers only.
        if has_bukkit_markers(&level) {
            Layout::PaperModern
        } else {
            Layout::VanillaModern
        }
    } else if level.is_dir() {
        // Vanilla-like is the default when a single world dir exists
        // (Fabric/Forge/Vanilla are indistinguishable on disk).
        Layout::Vanilla
    } else {
        Layout::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("mcsc-detect-{}", std::process::id()));
        let _ = fs::create_dir_all(&d);
        d
    }

    #[test]
    fn detects_legacy() {
        let root = tmp().join("legacy");
        fs::create_dir_all(root.join("world_nether").join("DIM-1")).unwrap();
        fs::create_dir_all(root.join("world_the_end").join("DIM1")).unwrap();
        fs::create_dir_all(root.join("world")).unwrap();
        fs::write(root.join("world").join("level.dat"), b"x").unwrap();
        let d = detect_layout(&root, "world");
        assert_eq!(d.layout, Layout::LegacySplit);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_paper_modern() {
        let root = tmp().join("modern");
        let dat = root.join("world/dimensions/minecraft/overworld/data/minecraft");
        fs::create_dir_all(&dat).unwrap();
        fs::write(dat.join("weather.dat"), b"x").unwrap();
        fs::create_dir_all(root.join("world")).unwrap();
        fs::write(root.join("world").join("level.dat"), b"x").unwrap();
        // Same tree without Bukkit markers reads as vanilla 26.1+.
        let d = detect_layout(&root, "world");
        assert_eq!(d.layout, Layout::VanillaModern);
        // A paper-world.yml marker flips it to Paper modern.
        fs::write(
            root.join("world/dimensions/minecraft/overworld/paper-world.yml"),
            b"x",
        )
        .unwrap();
        let d = detect_layout(&root, "world");
        assert_eq!(d.layout, Layout::PaperModern);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_paper_modern_terrain() {
        let root = tmp().join("modern-terrain");
        let reg = root.join("world/dimensions/minecraft/overworld/region");
        fs::create_dir_all(&reg).unwrap();
        fs::write(reg.join("r.0.0.mca"), b"x").unwrap();
        fs::create_dir_all(root.join("world")).unwrap();
        fs::write(root.join("world").join("level.dat"), b"x").unwrap();
        // No Bukkit markers: vanilla 26.1+ layout.
        let d = detect_layout(&root, "world");
        assert_eq!(d.layout, Layout::VanillaModern);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn paper_leftovers_are_vanilla() {
        // After converting, only Paper-only files remain under dimensions/.
        // Those must not keep the world classified as Paper modern.
        let root = tmp().join("leftovers");
        let base = root.join("world/dimensions/minecraft/overworld");
        fs::create_dir_all(base.join("data").join("paper")).unwrap();
        fs::write(base.join("paper-world.yml"), b"x").unwrap();
        fs::create_dir_all(root.join("world").join("region")).unwrap();
        fs::write(root.join("world").join("level.dat"), b"x").unwrap();
        let d = detect_layout(&root, "world");
        assert_eq!(d.layout, Layout::Vanilla);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_multiple_worlds_and_attaches_mirrors() {
        let root = tmp().join("multi");
        for w in ["world", "world2"] {
            fs::create_dir_all(root.join(w)).unwrap();
            fs::write(root.join(w).join("level.dat"), b"x").unwrap();
        }
        fs::create_dir_all(root.join("world_nether").join("DIM-1")).unwrap();
        fs::write(root.join("world_nether").join("level.dat"), b"x").unwrap();
        fs::create_dir_all(root.join("junk")).unwrap();
        let worlds = find_worlds(&root);
        let names: Vec<_> = worlds.iter().map(|w| w.level_name.as_str()).collect();
        // world_nether attaches to world instead of listing alone.
        assert_eq!(names, vec!["world", "world2"]);
        assert_eq!(worlds[0].mirrors, vec!["world_nether".to_string()]);
        assert_eq!(worlds[0].layout, Layout::LegacySplit);
        assert_eq!(worlds[1].layout, Layout::Vanilla);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn detects_vanilla() {
        let root = tmp().join("vanilla");
        fs::create_dir_all(root.join("world").join("DIM-1")).unwrap();
        fs::write(root.join("world").join("level.dat"), b"x").unwrap();
        let d = detect_layout(&root, "world");
        assert_eq!(d.layout, Layout::Vanilla);
        let _ = fs::remove_dir_all(&root);
    }
}
