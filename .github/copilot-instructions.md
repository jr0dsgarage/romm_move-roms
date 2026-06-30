# Copilot Instructions

## Runtime and UX
- Keep the scan and transfer phases responsive by doing long-running work on a worker thread and reporting progress back to the TUI.
- Prefer lightweight progress updates that summarize the current file and destination rather than doing extra per-item work in the UI thread.
- Preserve cancelability for both scan and transfer progress screens.

## Destination Layout
- Single-file ROMs such as `.zip`, `.7z`, `.smc`, `.gba`, and similar should be placed directly in the platform stub folder under `<source>/roms/<platform-slug>`.
- Do not add an extra inferred game-name subfolder for those single-file entries.
- Only append a RomM category subfolder when the source path clearly indicates one, such as `patch`, `manual`, `update`, `dlc`, `hack`, `mod`, `demo`, `translation`, or `prototype`.

## Change Discipline
- Prefer small, targeted edits over broad refactors when adjusting path planning or TUI behavior.
- If a helper becomes unused after a placement or layout change, remove it instead of leaving dead code behind.
- Validate any path-routing change with a focused unit test that shows the exact destination shape.