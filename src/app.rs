use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use anyhow::{bail, Context, Result};

use crate::{
    classifier::Classifier,
    model::{PlannedMove, Summary, TransferMode},
    scanner,
    tui,
};

struct PreparedData {
    plan: Vec<PlannedMove>,
    summary: Summary,
}

pub fn run(source_root: PathBuf, transfer_mode: TransferMode) -> Result<()> {
    let source_root = source_root
        .canonicalize()
        .with_context(|| format!("Source path is not accessible: {}", source_root.display()))?;

    if !source_root.is_dir() {
        bail!("Source path must be a directory: {}", source_root.display());
    }

    let output_root = source_root.join("roms");

    let classifier = Classifier::from_embedded()?;
    let worker_source = source_root.clone();
    let worker_output = output_root.clone();

    let (progress_tx, progress_rx) = mpsc::channel();
    let worker = thread::spawn(move || -> Result<PreparedData> {
        const PHASE_TOTAL: usize = 2;
        let allowed_extensions = classifier.allowed_extensions();

        let scan_total = scanner::count_scan_candidates(&worker_source, &worker_output)?;
        let (files, skipped) = scanner::scan_files_with_total(
            &worker_source,
            &worker_output,
            &allowed_extensions,
            scan_total,
            |progress| {
                let _ = progress_tx.send(tui::LoadingUpdate {
                    phase: String::from("Scanning folders"),
                    phase_index: 1,
                    phase_total: PHASE_TOTAL,
                    current: progress.current_path.display().to_string(),
                    processed: progress.processed,
                    total: progress.total,
                });
            },
        )?;

        let mut plan = Vec::with_capacity(files.len());
        for (index, source) in files.iter().enumerate() {
            let file_ext = source
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();

            let phase = if file_ext == "zip" {
                "Inspecting zip/classifying"
            } else {
                "Classifying files"
            };

            let _ = progress_tx.send(tui::LoadingUpdate {
                phase: String::from(phase),
                phase_index: 2,
                phase_total: PHASE_TOTAL,
                current: source.display().to_string(),
                processed: index + 1,
                total: files.len(),
            });

            let classification = classifier.classify(source);
            let destination = classification.platform_slug.as_ref().and_then(|slug| {
                plan_destination_for_game(&worker_source, &worker_output, source, slug)
            });

            plan.push(PlannedMove {
                source: source.clone(),
                destination,
                platform_slug: classification.platform_slug,
                confidence: classification.confidence,
                reason: classification.reason,
                has_conflict: false,
            });
        }

        mark_conflicts(&mut plan);
        let summary = summarize(&plan, skipped);

        Ok(PreparedData { plan, summary })
    });

    tui::run_loading_modal(progress_rx)?;

    let prepared = worker
        .join()
        .map_err(|_| anyhow::anyhow!("Scan worker thread panicked"))??;

    let selection = tui::run(tui::AppView {
        source_root,
        output_root,
        plan: prepared.plan,
        summary: prepared.summary,
        transfer_mode,
    })?;

    if !selection.confirmed {
        println!("{} canceled. No files were transferred.", transfer_mode.prompt_label());
        return Ok(());
    }

    let stats = execute_transfers(
        &selection.plan,
        &selection.disabled_plan_indices,
        transfer_mode,
    )?;

    println!(
        "{} complete: {} transferred | {} skipped (unclassified/conflict/disabled)",
        transfer_mode.prompt_label(),
        stats.transferred,
        stats.skipped
    );

    Ok(())
}

struct TransferStats {
    transferred: usize,
    skipped: usize,
}

fn execute_transfers(
    plan: &[PlannedMove],
    disabled_plan_indices: &HashSet<usize>,
    transfer_mode: TransferMode,
) -> Result<TransferStats> {
    let mut transferred = 0usize;
    let mut skipped = 0usize;

    for (index, item) in plan.iter().enumerate() {
        if disabled_plan_indices.contains(&index) {
            skipped += 1;
            continue;
        }

        let Some(destination) = &item.destination else {
            skipped += 1;
            continue;
        };

        if item.has_conflict {
            skipped += 1;
            continue;
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed creating destination folder: {}", parent.display())
            })?;
        }

        match transfer_mode {
            TransferMode::Copy => {
                fs::copy(&item.source, destination).with_context(|| {
                    format!(
                        "Failed copying {} -> {}",
                        item.source.display(),
                        destination.display()
                    )
                })?;
            }
            TransferMode::Move => {
                move_file(&item.source, destination)?;
            }
        }

        transferred += 1;
    }

    Ok(TransferStats { transferred, skipped })
}

fn move_file(source: &Path, destination: &Path) -> Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(source, destination).with_context(|| {
                format!(
                    "Failed copying during move {} -> {}",
                    source.display(),
                    destination.display()
                )
            })?;
            fs::remove_file(source)
                .with_context(|| format!("Failed removing moved source: {}", source.display()))?;
            Ok(())
        }
    }
}

fn plan_destination_for_game(
    source_root: &Path,
    output_root: &Path,
    source_file: &Path,
    slug: &str,
) -> Option<PathBuf> {
    let file_name = source_file.file_name()?;
    let rel = source_file.strip_prefix(source_root).ok()?;

    let game_folder = infer_game_folder_name(rel, source_file, slug)?;
    let mut destination = output_root.join(slug).join(game_folder);

    if let Some(category) = detect_romm_category(rel) {
        destination = destination.join(category);
    }

    Some(destination.join(file_name))
}

fn infer_game_folder_name(relative_path: &Path, source_file: &Path, slug: &str) -> Option<String> {
    let mut components: Vec<String> = relative_path
        .iter()
        .filter_map(|c| c.to_str())
        .map(|s| s.to_string())
        .collect();

    if components.is_empty() {
        return None;
    }

    // remove filename component
    components.pop();

    // Prefer the closest meaningful parent folder as game name when present.
    let parent_candidate = components
        .iter()
        .rev()
        .find(|component| {
            !is_non_game_container(component)
                && !is_platform_marker(component, slug)
                && !is_library_bucket(component)
        })
        .cloned();

    let raw = if let Some(parent) = parent_candidate {
        parent
    } else {
        source_file
            .file_stem()
            .and_then(|s| s.to_str())
            .map(normalize_game_name)
            .unwrap_or_else(|| String::from("unknown-game"))
    };

    Some(sanitize_folder_name(&raw))
}

fn detect_romm_category(relative_path: &Path) -> Option<String> {
    for component in relative_path.iter().filter_map(|c| c.to_str()) {
        let lower = component.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "dlc"
                | "hack"
                | "manual"
                | "mod"
                | "patch"
                | "update"
                | "demo"
                | "translation"
                | "prototype"
        ) {
            return Some(lower);
        }
    }

    None
}

fn is_non_game_container(component: &str) -> bool {
    let lower = component.to_ascii_lowercase();

    matches!(
        lower.as_str(),
        "rom"
            | "roms"
            | "bios"
            | "cdi"
            | "gdi"
            | "iso"
            | "bin"
            | "cue"
            | "chd"
            | "img"
            | "dvd"
            | "cd"
            | "disc"
            | "track"
            | "dlc"
            | "hack"
            | "manual"
            | "mod"
            | "patch"
            | "update"
            | "demo"
            | "translation"
            | "prototype"
    )
}

fn is_platform_marker(component: &str, slug: &str) -> bool {
    let component_norm = normalize_for_compare(component);
    let slug_norm = normalize_for_compare(slug);

    if component_norm.is_empty() || slug_norm.is_empty() {
        return false;
    }

    component_norm == slug_norm
        || component_norm.contains(&slug_norm)
        || slug_norm.contains(&component_norm)
}

fn is_library_bucket(component: &str) -> bool {
    let lower = component.trim().to_ascii_lowercase();

    matches!(lower.as_str(), "0day" | "0-day" | "1g1r" | "collection") || is_range_bucket(&lower)
}

fn is_range_bucket(component: &str) -> bool {
    let compact: String = component
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();

    if compact.len() == 1 {
        return compact.chars().all(|ch| ch.is_ascii_alphanumeric());
    }

    if compact.len() == 3 {
        let mut chars = compact.chars();
        let first = chars.next().unwrap_or_default();
        let middle = chars.next().unwrap_or_default();
        let last = chars.next().unwrap_or_default();

        return first.is_ascii_alphanumeric()
            && last.is_ascii_alphanumeric()
            && matches!(middle, '-' | '_' | '.');
    }

    compact == "0-9"
}

fn normalize_for_compare(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn normalize_game_name(input: &str) -> String {
    let mut name = input.replace('_', " ").replace('.', " ");

    // Strip common multi-disc/track suffix tokens from inferred game-folder names.
    let tokens_to_strip = [
        " track ",
        " disc ",
        " disk ",
        " cd ",
        " dvd ",
        " side ",
    ];

    for token in tokens_to_strip {
        if let Some(index) = name.to_ascii_lowercase().find(token) {
            name = name[..index].to_string();
        }
    }

    name.trim().to_string()
}

fn sanitize_folder_name(input: &str) -> String {
    let mut clean = String::new();
    for ch in input.chars() {
        if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            continue;
        }
        clean.push(ch);
    }

    let trimmed = clean.trim().trim_end_matches('.').trim_end_matches(' ');
    if trimmed.is_empty() {
        String::from("unknown-game")
    } else {
        trimmed.to_string()
    }
}

fn mark_conflicts(plan: &mut [PlannedMove]) {
    let mut seen = HashSet::new();

    for item in plan {
        let Some(destination) = &item.destination else {
            continue;
        };

        if !seen.insert(destination.clone()) {
            item.has_conflict = true;
        }
    }
}

fn summarize(plan: &[PlannedMove], skipped: usize) -> Summary {
    let mut summary = Summary {
        scanned_files: plan.len(),
        skipped,
        ..Summary::default()
    };

    for item in plan {
        if item.platform_slug.is_some() {
            summary.planned_moves += 1;
        } else {
            summary.unclassified += 1;
        }

        if item.has_conflict {
            summary.conflicts += 1;
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{detect_romm_category, infer_game_folder_name};

    #[test]
    fn falls_back_to_file_stem_for_platform_container_paths() {
        let relative = Path::new(r"0day\Atari 2600\roms\2Pak.bin");
        let source = Path::new(r"0day\Atari 2600\roms\2Pak.bin");

        let game = infer_game_folder_name(relative, source, "atari2600");

        assert_eq!(game.as_deref(), Some("2Pak"));
    }

    #[test]
    fn falls_back_to_archive_stem_for_bucketed_library_paths() {
        let relative = Path::new(r"GBA ENGLISH\GBA\A-D\Disney.7z");
        let source = Path::new(r"GBA ENGLISH\GBA\A-D\Disney.7z");

        let game = infer_game_folder_name(relative, source, "gba");

        assert_eq!(game.as_deref(), Some("Disney"));
    }

    #[test]
    fn keeps_meaningful_parent_folder_as_game_name() {
        let relative = Path::new(r"240pTestSuite\240pTS.smc");
        let source = Path::new(r"240pTestSuite\240pTS.smc");

        let game = infer_game_folder_name(relative, source, "snes");

        assert_eq!(game.as_deref(), Some("240pTestSuite"));
    }

    #[test]
    fn detects_documented_multifile_categories() {
        let relative = Path::new(r"Some Game\patch\fix.zip");

        assert_eq!(detect_romm_category(relative).as_deref(), Some("patch"));
    }
}
