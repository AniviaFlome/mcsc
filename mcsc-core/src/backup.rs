use chrono::Local;
use std::path::{Path, PathBuf};

fn timestamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Copy `dir` to `<dir>.bak-<timestamp>` (sibling of original).
/// Returns the backup path. Copies recursively; fails on first IO error.
/// When a backup for the current second already exists (rapid repeated
/// runs), a `-2`, `-3`, … suffix is appended so backups never merge.
pub fn backup_world_dir(dir: &Path) -> std::io::Result<PathBuf> {
    let name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "world".to_string());
    let base = format!("{}.bak-{}", name, timestamp());
    let mut dest = dir.with_file_name(&base);
    let mut n = 2u32;
    while dest.exists() {
        dest = dir.with_file_name(format!("{base}-{n}"));
        n += 1;
    }
    copy_dir_recursive(dir, &dest)?;
    Ok(dest)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else if ty.is_symlink() {
            // Recreate the link itself so the backup stays self-contained
            // without following it (a dangling or absolute link still copies).
            let target = std::fs::read_link(entry.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &to)?;
            #[cfg(not(unix))]
            std::fs::copy(entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_backups_never_share_a_path() {
        let parent = std::env::temp_dir().join(format!("mcsc-bak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let dir = parent.join("world");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f.txt"), b"hi").unwrap();
        let b1 = backup_world_dir(&dir).unwrap();
        let b2 = backup_world_dir(&dir).unwrap();
        assert_ne!(b1, b2, "same-second backups must not collide");
        assert!(b1.join("f.txt").is_file());
        assert!(b2.join("f.txt").is_file());
        let _ = std::fs::remove_dir_all(&parent);
    }
}
