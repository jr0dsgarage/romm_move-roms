use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use anyhow::{bail, Context, Result};

use crate::{
    classifier::Classifier,
    model::{PlannedMove, Summary},
    scanner,
    tui,
};

struct PreparedData {
    plan: Vec<PlannedMove>,
    summary: Summary,
}

pub fn run(source_root: PathBuf) -> Result<()> {
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

    tui::run(tui::AppView {
        source_root,
        output_root,
        plan: prepared.plan,
        summary: prepared.summary,
    })
}

fn plan_destination_for_game(
    source_root: &Path,
    output_root: &Path,
    source_file: &Path,
    slug: &str,
) -> Option<PathBuf> {
    let file_name = source_file.file_name()?;
    let rel = source_file.strip_prefix(source_root).ok()?;

    let game_folder = infer_game_folder_name(rel, source_file)?;
    let mut destination = output_root.join(slug).join(game_folder);

    if let Some(category) = detect_romm_category(rel) {
        destination = destination.join(category);
    }

    Some(destination.join(file_name))
}

fn infer_game_folder_name(relative_path: &Path, source_file: &Path) -> Option<String> {
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
        .find(|component| !is_non_game_container(component))
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
        "roms"
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
