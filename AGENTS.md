# AGENTS.md

## Purpose
This repository contains a Rust CLI/TUI tool that scans a RomM source library, previews planned file placement, and then copies or moves the selected files into the RomM output structure.

## Project Snapshot
- Language: Rust 2024 edition.
- Entry point: `src/main.rs`.
- UI stack: `ratatui` + `crossterm`.
- Scan and classification helpers: `src/scanner.rs` and `src/classifier.rs`.
- Destination planning and transfer execution: `src/app.rs`.
- Interactive TUI: `src/tui.rs`.

## Canonical Commands
- Build/check: `cargo check`
- Test: `cargo test`
- Run: `cargo run -- <source-root>`
- Move mode: `cargo run -- <source-root> -m`
- Optional formatting: `cargo fmt`
- Optional linting: `cargo clippy`

## Working Rules
- Prefer small, focused edits over broad refactors.
- Keep the TUI responsive; any long-running work should stay on a worker thread with progress updates.
- Treat warnings seriously when they point to dead code, stale helpers, or path-routing drift.
- When changing placement logic, add or update a unit test that shows the exact destination path.
- Do not leave unused helpers behind after changing layout or routing; remove them once they are no longer needed.

## Destination Layout Rules
- The tool writes output under `<source-root>/roms/<platform-slug>`.
- Single-file ROMs such as `.zip`, `.7z`, `.smc`, `.gba`, and similar should go directly into that platform stub folder.
- Do not add an extra inferred game-name subfolder for those single-file entries.
- Only add a RomM category subfolder when the source path clearly indicates one, such as `patch`, `manual`, `update`, `dlc`, `hack`, `mod`, `demo`, `translation`, or `prototype`.
- If a path contains conflicting indicators, prefer the simpler flat placement over inventing another nested folder.

## File Ownership
- `src/main.rs`: CLI parsing and mode selection.
- `src/app.rs`: orchestration, scan worker, transfer worker, destination planning, conflict detection, and final execution.
- `src/classifier.rs`: platform classification and archive inspection.
- `src/scanner.rs`: filesystem traversal and filtering.
- `src/model.rs`: shared data structures and transfer mode definitions.
- `src/tui.rs`: loading modal, confirmation UI, progress UI, selection state, and mouse/key handling.

## TUI/Transfer Behavior
- The loading modal should stay visible while scanning and should allow canceling the scan.
- The transfer modal should stay visible while copying or moving files, show the current source and destination, and allow canceling between files.
- The confirmation screen is the user’s last pre-transfer review point; the app should surface mode-aware wording there.
- Preserve the current selection model and avoid widening the UI surface unless the change is directly related to placement, progress, or confirmation.

## Validation Expectations
- Run `cargo check` after code changes that touch compile paths.
- Run `cargo test` after changing placement logic, classification, or any code with behavioral impact.
- If a change affects the TUI layout or progress flow, verify the code compiles and, when possible, exercise the screen manually.

## Common Pitfalls
- Do not reintroduce the old game-folder inference path for single-file ROMs.
- Be careful with UNC and Windows path normalization; the UI should display readable paths without `\\?\` prefixes.
- Do not assume the user wants dry-run behavior only; the app now performs real copy/move execution after confirmation.
- Avoid blocking the UI thread with file I/O or archive inspection.
- Keep file-routing changes aligned with the existing tests in `src/app.rs`.
