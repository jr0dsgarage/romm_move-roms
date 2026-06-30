use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
        Arc,
    },
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
    let scan_cancelled = Arc::new(AtomicBool::new(false));

    let (progress_tx, progress_rx) = mpsc::channel();
    let scan_cancelled_worker = Arc::clone(&scan_cancelled);
    let worker = thread::spawn(move || -> Result<PreparedData> {
        const PHASE_TOTAL: usize = 2;
        let allowed_extensions = classifier.allowed_extensions();

        let scan_total = scanner::count_scan_candidates(
            &worker_source,
            &worker_output,
            &scan_cancelled_worker,
        )?;
        let (files, skipped) = scanner::scan_files_with_total(
            &worker_source,
            &worker_output,
            &allowed_extensions,
            scan_total,
            &scan_cancelled_worker,
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

    tui::run_loading_modal(progress_rx, Arc::clone(&scan_cancelled))?;

    let prepared = match worker.join() {
        Ok(result) => match result {
            Ok(prepared) => prepared,
            Err(error) if error.to_string() == "Operation canceled" => {
                println!("Scan canceled. No files were transferred.");
                return Ok(());
            }
            Err(error) => return Err(error),
        },
        Err(_) => return Err(anyhow::anyhow!("Scan worker thread panicked")),
    };

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

    let transfer_cancelled = Arc::new(AtomicBool::new(false));
    let (transfer_tx, transfer_rx) = mpsc::channel();
    let transfer_mode_label = transfer_mode.prompt_label();
    let transfer_cancelled_worker = Arc::clone(&transfer_cancelled);
    let transfer_plan = selection.plan;
    let transfer_disabled = selection.disabled_plan_indices;
    let transfer_worker = thread::spawn(move || {
        execute_transfers(
            &transfer_plan,
            &transfer_disabled,
            transfer_mode,
            transfer_tx,
            transfer_cancelled_worker,
        )
    });

    tui::run_transfer_modal(transfer_rx, Arc::clone(&transfer_cancelled), transfer_mode_label)?;

    let stats = match transfer_worker.join() {
        Ok(result) => match result {
            Ok(stats) => stats,
            Err(error) if error.to_string() == "Operation canceled" => {
                println!("{} canceled during transfer.", transfer_mode.prompt_label());
                return Ok(());
            }
            Err(error) => return Err(error),
        },
        Err(_) => return Err(anyhow::anyhow!("Transfer worker thread panicked")),
    };

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
    progress_tx: mpsc::Sender<tui::TransferUpdate>,
    cancelled: Arc<AtomicBool>,
) -> Result<TransferStats> {
    let mut transferred = 0usize;
    let mut skipped = 0usize;

    for (index, item) in plan.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            return Err(anyhow::anyhow!("Operation canceled"));
        }

        if disabled_plan_indices.contains(&index) {
            skipped += 1;
            let _ = progress_tx.send(tui::TransferUpdate {
                phase: format!("{}", transfer_mode.prompt_label()),
                source: item.source.display().to_string(),
                destination: item
                    .destination
                    .as_ref()
                    .map(|dst| dst.display().to_string())
                    .unwrap_or_default(),
                processed: index + 1,
                total: plan.len(),
                transferred,
                skipped,
            });
            continue;
        }

        let Some(destination) = &item.destination else {
            skipped += 1;
            let _ = progress_tx.send(tui::TransferUpdate {
                phase: format!("{}", transfer_mode.prompt_label()),
                source: item.source.display().to_string(),
                destination: String::new(),
                processed: index + 1,
                total: plan.len(),
                transferred,
                skipped,
            });
            continue;
        };

        if item.has_conflict {
            skipped += 1;
            let _ = progress_tx.send(tui::TransferUpdate {
                phase: format!("{}", transfer_mode.prompt_label()),
                source: item.source.display().to_string(),
                destination: destination.display().to_string(),
                processed: index + 1,
                total: plan.len(),
                transferred,
                skipped,
            });
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

        let _ = progress_tx.send(tui::TransferUpdate {
            phase: format!("{}", transfer_mode.prompt_label()),
            source: item.source.display().to_string(),
            destination: destination.display().to_string(),
            processed: index + 1,
            total: plan.len(),
            transferred: transferred + 1,
            skipped,
        });

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

    let mut destination = output_root.join(slug);

    if let Some(category) = detect_romm_category(rel) {
        destination = destination.join(category);
    }

    Some(destination.join(file_name))
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

    use super::{detect_romm_category, plan_destination_for_game};

    #[test]
    fn detects_documented_multifile_categories() {
        let relative = Path::new(r"Some Game\patch\fix.zip");

        assert_eq!(detect_romm_category(relative).as_deref(), Some("patch"));
    }

    #[test]
    fn places_single_file_roms_directly_in_stub_folder() {
        let source_root = Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast");
        let output_root = Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast\roms");
        let source = Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast\Sonic Adventure.bin");

        let destination = super::plan_destination_for_game(
            source_root,
            output_root,
            source,
            "dreamcast",
        );

        assert_eq!(
            destination.as_deref(),
            Some(Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast\roms\dreamcast\Sonic Adventure.bin"))
        );
    }

    #[test]
    fn keeps_category_subfolders_for_multi_file_romm_types() {
        let source_root = Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast");
        let output_root = Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast\roms");
        let source = Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast\Some Game\patch\fix.zip");

        let destination = plan_destination_for_game(source_root, output_root, source, "dreamcast");

        assert_eq!(
            destination.as_deref(),
            Some(Path::new(r"\\Vesuvius\emulation\ROMS\Dreamcast\roms\dreamcast\patch\fix.zip"))
        );
    }
}
