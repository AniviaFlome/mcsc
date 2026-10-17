use chrono::Local;
use std::path::{Path, PathBuf};

fn timestamp() -> String {
    Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Copy `dir` to `<dir>.bak-<timestamp>` (sibling of original).
/// Returns the backup path. Copies recursively; fails on first IO error.
pub fn backup_world_dir(dir: &Path) -> std::io::Result<PathBuf> {
    let name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "world".to_string());
    let dest = dir.with_file_name(format!("{}.bak-{}", name, timestamp()));
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
            // Resolve symlinks to regular copies to keep backup self-contained.
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
