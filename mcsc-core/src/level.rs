use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

/// Minimal level.dat reader: gunzips then parses NBT via fastnbt,
/// returning DataVersion and LevelName when present.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelInfo {
    pub data_version: Option<i32>,
    pub level_name: Option<String>,
    pub game_type: Option<String>,
}

pub fn read_level_info(level_dat: &Path) -> Result<LevelInfo, Box<dyn std::error::Error>> {
    let bytes = gunzip_if_needed(level_dat)?;
    // fastnbt Value round-trip
    let value: fastnbt::Value = fastnbt::from_bytes(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(extract_level_info(&value))
}

fn gunzip_if_needed(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    File::open(path)?.read_to_end(&mut raw)?;
    // gzip magic 1f 8b
    if raw.len() >= 2 && raw[0] == 0x1f && raw[1] == 0x8b {
        let mut dec = flate2::read::GzDecoder::new(&raw[..]);
        let mut out = Vec::new();
        dec.read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(raw)
    }
}

fn extract_level_info(root: &fastnbt::Value) -> LevelInfo {
    let mut info = LevelInfo::default();
    // Root is Compound with "Data" compound.
    let data = match root {
        fastnbt::Value::Compound(map) => map.get("Data"),
        _ => None,
    };
    let Some(fastnbt::Value::Compound(data)) = data else {
        return info;
    };
    if let Some(fastnbt::Value::Int(v)) = data.get("DataVersion") {
        info.data_version = Some(*v);
    }
    if let Some(fastnbt::Value::String(s)) = data.get("LevelName") {
        info.level_name = Some(s.clone());
    }
    info
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedInjection {
    /// WorldGenSettings written into level.dat.
    Injected { seed: i64 },
    /// level.dat already had WorldGenSettings; untouched.
    AlreadyPresent,
    /// No world_gen_settings.dat source found; nothing to do.
    NoSource,
}

/// Paper 26.1+ keeps the world seed *outside* level.dat, in
/// `data/minecraft/world_gen_settings.dat`, which vanilla ignores. Without
/// this step vanilla generates fresh worldgen (different seed, relocated
/// spawn). Copies the `data` compound into `level.dat/Data/WorldGenSettings`
/// when missing. In dry-run mode only reports what would happen.
pub fn ensure_world_gen_settings(
    level_dat: &Path,
    wgs_dat: &Path,
    dry_run: bool,
) -> Result<SeedInjection, Box<dyn std::error::Error>> {
    if !wgs_dat.is_file() {
        return Ok(SeedInjection::NoSource);
    }
    let wgs_bytes = gunzip_if_needed(wgs_dat)?;
    let wgs: fastnbt::Value = fastnbt::from_bytes(&wgs_bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let Some(wgs_data) = wgs_compound(&wgs, &["data"]) else {
        return Ok(SeedInjection::NoSource);
    };
    let seed = match wgs_data.get("seed") {
        Some(fastnbt::Value::Long(s)) => *s,
        _ => return Ok(SeedInjection::NoSource),
    };

    let level_bytes = gunzip_if_needed(level_dat)?;
    let mut level: fastnbt::Value = fastnbt::from_bytes(&level_bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let Some(data) = wgs_compound_mut(&mut level, &["Data"]) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} has no Data compound", level_dat.display()),
        )
        .into());
    };
    if data.contains_key("WorldGenSettings") {
        return Ok(SeedInjection::AlreadyPresent);
    }
    let action = SeedInjection::Injected { seed };
    if dry_run {
        return Ok(action);
    }
    data.insert(
        "WorldGenSettings".to_string(),
        fastnbt::Value::Compound(wgs_data.clone()),
    );
    let raw = fastnbt::to_bytes(&level)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let file = File::create(level_dat)?;
    let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    enc.write_all(&raw)?;
    enc.finish()?;
    Ok(action)
}

fn wgs_compound<'a>(
    v: &'a fastnbt::Value,
    path: &[&str],
) -> Option<&'a HashMap<String, fastnbt::Value>> {
    let mut cur = v;
    for key in path {
        match cur {
            fastnbt::Value::Compound(map) => cur = map.get(*key)?,
            _ => return None,
        }
    }
    match cur {
        fastnbt::Value::Compound(map) => Some(map),
        _ => None,
    }
}

fn wgs_compound_mut<'a>(
    v: &'a mut fastnbt::Value,
    path: &[&str],
) -> Option<&'a mut HashMap<String, fastnbt::Value>> {
    let mut cur = v;
    for key in path {
        match cur {
            fastnbt::Value::Compound(map) => cur = map.get_mut(*key)?,
            _ => return None,
        }
    }
    match cur {
        fastnbt::Value::Compound(map) => Some(map),
        _ => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_gz(path: &Path, value: &fastnbt::Value) {
        let raw = fastnbt::to_bytes(value).unwrap();
        let file = File::create(path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();
    }

    fn compound(pairs: Vec<(&str, fastnbt::Value)>) -> fastnbt::Value {
        fastnbt::Value::Compound(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mcsc-level-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Minimal Paper-style level.dat: Data without WorldGenSettings.
        write_gz(
            &dir.join("level.dat"),
            &compound(vec![(
                "Data",
                compound(vec![
                    ("DataVersion", fastnbt::Value::Int(4903)),
                    ("LevelName", fastnbt::Value::String("world".into())),
                ]),
            )]),
        );
        // Paper world_gen_settings.dat holding the seed.
        write_gz(
            &dir.join("world_gen_settings.dat"),
            &compound(vec![
                ("DataVersion", fastnbt::Value::Int(4903)),
                (
                    "data",
                    compound(vec![
                        ("seed", fastnbt::Value::Long(7958551649708054444)),
                        ("generate_structures", fastnbt::Value::Byte(1)),
                        ("bonus_chest", fastnbt::Value::Byte(0)),
                    ]),
                ),
            ]),
        );
        dir
    }

    fn read_seed(level_dat: &Path) -> Option<fastnbt::Value> {
        let back: fastnbt::Value =
            fastnbt::from_bytes(&gunzip_if_needed(level_dat).unwrap()).unwrap();
        let data = wgs_compound(&back, &["Data"])?;
        let wgs = match data.get("WorldGenSettings") {
            Some(fastnbt::Value::Compound(m)) => m,
            _ => return None,
        };
        wgs.get("seed").cloned()
    }

    #[test]
    fn injects_seed_when_missing() {
        let dir = fixture("inject");
        let out = ensure_world_gen_settings(
            &dir.join("level.dat"),
            &dir.join("world_gen_settings.dat"),
            false,
        )
        .unwrap();
        assert_eq!(
            out,
            SeedInjection::Injected {
                seed: 7958551649708054444
            }
        );
        assert_eq!(
            read_seed(&dir.join("level.dat")),
            Some(fastnbt::Value::Long(7958551649708054444))
        );
        // Original tags preserved.
        let back: fastnbt::Value =
            fastnbt::from_bytes(&gunzip_if_needed(&dir.join("level.dat")).unwrap()).unwrap();
        let data = wgs_compound(&back, &["Data"]).unwrap();
        assert_eq!(
            data.get("LevelName"),
            Some(&fastnbt::Value::String("world".into()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_when_already_present() {
        let dir = fixture("present");
        ensure_world_gen_settings(
            &dir.join("level.dat"),
            &dir.join("world_gen_settings.dat"),
            false,
        )
        .unwrap();
        let before = std::fs::read(dir.join("level.dat")).unwrap();
        let out = ensure_world_gen_settings(
            &dir.join("level.dat"),
            &dir.join("world_gen_settings.dat"),
            false,
        )
        .unwrap();
        assert_eq!(out, SeedInjection::AlreadyPresent);
        assert_eq!(std::fs::read(dir.join("level.dat")).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = fixture("dry");
        let before = std::fs::read(dir.join("level.dat")).unwrap();
        let out = ensure_world_gen_settings(
            &dir.join("level.dat"),
            &dir.join("world_gen_settings.dat"),
            true,
        )
        .unwrap();
        assert_eq!(
            out,
            SeedInjection::Injected {
                seed: 7958551649708054444
            }
        );
        assert_eq!(std::fs::read(dir.join("level.dat")).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_source_without_dat_file() {
        let dir = std::env::temp_dir().join(format!("mcsc-level-nosrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_gz(
            &dir.join("level.dat"),
            &compound(vec![("Data", compound(vec![]))]),
        );
        let out =
            ensure_world_gen_settings(&dir.join("level.dat"), &dir.join("missing.dat"), false)
                .unwrap();
        assert_eq!(out, SeedInjection::NoSource);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A world-generation bundle usable for a top-level `world_gen_settings.dat`
/// file: full dimension configs plus the data version for the file root.
#[derive(Debug, Clone)]
pub struct SeedBundle {
    pub data_version: Option<i32>,
    pub wgs: HashMap<String, fastnbt::Value>,
}

/// Extract a seed bundle from a legacy level.dat (1.20.5+ `WorldGenSettings`
/// compound). Ancient `RandomSeed`-only files cannot produce the full
/// dimension configs vanilla expects, so those return None (caller warns).
pub fn extract_seed_bundle(
    level_dat: &Path,
) -> Result<Option<SeedBundle>, Box<dyn std::error::Error>> {
    let bytes = gunzip_if_needed(level_dat)?;
    let root: fastnbt::Value = fastnbt::from_bytes(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let Some(data) = wgs_compound(&root, &["Data"]) else {
        return Ok(None);
    };
    let data_version = match data.get("DataVersion") {
        Some(fastnbt::Value::Int(v)) => Some(*v),
        _ => None,
    };
    match data.get("WorldGenSettings") {
        Some(fastnbt::Value::Compound(wgs)) => Ok(Some(SeedBundle {
            data_version,
            wgs: wgs.clone(),
        })),
        _ => Ok(None),
    }
}

/// Write a top-level `world_gen_settings.dat` file from a bundle.
pub fn write_seed_file(dest: &Path, bundle: &SeedBundle, dry_run: bool) -> std::io::Result<bool> {
    if !dry_run {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut root = HashMap::new();
        if let Some(v) = bundle.data_version {
            root.insert("DataVersion".to_string(), fastnbt::Value::Int(v));
        }
        root.insert(
            "data".to_string(),
            fastnbt::Value::Compound(bundle.wgs.clone()),
        );
        let raw = fastnbt::to_bytes(&fastnbt::Value::Compound(root))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let file = File::create(dest)?;
        let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        enc.write_all(&raw)?;
        enc.finish()?;
    }
    Ok(true)
}

#[cfg(test)]
mod seed_bundle_tests {
    use super::*;

    #[test]
    fn extracts_full_bundle_and_skips_randomseed_only() {
        let dir = std::env::temp_dir().join(format!("mcsc-seedbundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut wgs = HashMap::new();
        wgs.insert("seed".to_string(), fastnbt::Value::Long(99));
        let mut data = HashMap::new();
        data.insert("DataVersion".to_string(), fastnbt::Value::Int(4903));
        data.insert(
            "WorldGenSettings".to_string(),
            fastnbt::Value::Compound(wgs),
        );
        let mut root = HashMap::new();
        root.insert("Data".to_string(), fastnbt::Value::Compound(data));
        let raw = fastnbt::to_bytes(&fastnbt::Value::Compound(root)).unwrap();
        let f = File::create(dir.join("level.dat")).unwrap();
        let mut enc = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        enc.write_all(&raw).unwrap();
        enc.finish().unwrap();

        let bundle = extract_seed_bundle(&dir.join("level.dat"))
            .unwrap()
            .expect("bundle");
        assert_eq!(bundle.data_version, Some(4903));
        assert_eq!(bundle.wgs.get("seed"), Some(&fastnbt::Value::Long(99)));
        // Round-trips through the file writer.
        let dest = dir.join("world_gen_settings.dat");
        write_seed_file(&dest, &bundle, false).unwrap();
        let back: fastnbt::Value = fastnbt::from_bytes(&gunzip_if_needed(&dest).unwrap()).unwrap();
        let data = wgs_compound(&back, &["data"]).unwrap();
        assert_eq!(data.get("seed"), Some(&fastnbt::Value::Long(99)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
