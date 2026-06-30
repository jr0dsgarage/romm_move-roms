use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Context, Result};
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub processed: usize,
    pub total: usize,
    pub current_path: PathBuf,
}

pub fn count_scan_candidates(
    source_root: &Path,
    output_root: &Path,
    cancelled: &AtomicBool,
) -> Result<usize> {
    let mut total = 0usize;

    let walker = WalkDir::new(source_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_descend(entry, output_root));

    for entry_result in walker {
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("Operation canceled");
        }

        let entry = entry_result.context("Failed while traversing source directory")?;
        let path = entry.path();

        if path.starts_with(output_root) {
            continue;
        }

        if entry.file_type().is_file() {
            total += 1;
        }
    }

    Ok(total)
}

pub fn scan_files_with_total<F>(
    source_root: &Path,
    output_root: &Path,
    allowed_extensions: &HashSet<String>,
    total: usize,
    cancelled: &AtomicBool,
    mut on_progress: F,
) -> Result<(Vec<PathBuf>, usize)>
where
    F: FnMut(ScanProgress),
{
    let mut files = Vec::new();
    let mut skipped = 0usize;
    let mut processed = 0usize;

    let walker = WalkDir::new(source_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_descend(entry, output_root));

    for entry_result in walker {
        if cancelled.load(Ordering::Relaxed) {
            anyhow::bail!("Operation canceled");
        }

        let entry = entry_result.context("Failed while traversing source directory")?;
        let path = entry.path();

        if path.starts_with(output_root) {
            skipped += 1;
            continue;
        }

        if !entry.file_type().is_file() {
            continue;
        }

        processed += 1;
        on_progress(ScanProgress {
            processed,
            total,
            current_path: path.to_path_buf(),
        });

        if should_skip_file(path) {
            skipped += 1;
            continue;
        }

        if !is_allowed_extension(path, allowed_extensions) {
            skipped += 1;
            continue;
        }

        files.push(path.to_path_buf());
    }

    Ok((files, skipped))
}

fn should_descend(entry: &DirEntry, output_root: &Path) -> bool {
    let path = entry.path();

    if path.starts_with(output_root) {
        return false;
    }

    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
        if name.starts_with('.') && entry.depth() > 0 {
            return false;
        }

        if matches!(name, "$RECYCLE.BIN" | "System Volume Information") {
            return false;
        }
    }

    true
}

fn should_skip_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return true;
    };

    if name.starts_with('.') {
        return true;
    }

    matches!(
        name.to_ascii_lowercase().as_str(),
        "thumbs.db" | "desktop.ini"
    )
}

fn is_allowed_extension(path: &Path, allowed_extensions: &HashSet<String>) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };

    allowed_extensions.contains(&ext.to_ascii_lowercase())
}
