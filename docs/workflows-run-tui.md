# TUI-First `meta agents workflows run`

## Summary

`meta agents workflows run <NAME>` is now TUI-first for interactive terminal sessions. When both stdin and stdout are attached to a TTY, the command opens a guided wizard, generates the workflow Markdown artifact, lands on a review/export dashboard, and lets the user optionally edit or save the result before exiting. `meta agents workflow run <NAME>` remains available as a compatibility alias.

The deterministic fallback remains available for scripts, CI, and tests via `--no-interactive`.

## Routing Contract

- Interactive path:
  - Triggered when stdin and stdout are TTYs and `--no-interactive` is not set.
  - `--render-once` also forces the TUI path, but renders one deterministic snapshot instead of entering the live terminal loop.
- Non-interactive path:
  - Triggered when `--no-interactive` is set.
  - Also used automatically when the command runs without a TTY and `--render-once` is not set.
  - Requires all required workflow inputs to be provided with `--param key=value`.
- Dry run:
  - `--dry-run` stays headless and renders the resolved prompt/instructions without opening the wizard.

## Wizard Steps

- One step per workflow parameter in front-matter order.
- Each step shows:
  - parameter name
  - required/optional status
  - description
  - current value editor
- Input behavior:
  - `Enter` advances to the next step.
  - On the last step, `Enter` generates the workflow artifact.
  - `Shift+Enter` inserts a newline inside the current field.
  - `Shift+Tab` moves back without losing prior values.
  - `Esc` or `Ctrl+C` cancels the run.

## Validation Rules

- Required parameters must be non-empty.
- The `linear_issue_parameter` field, and the conventional `issue` field, must look like a Linear identifier such as `MET-50`.
- Unknown `--param` keys are rejected before the wizard or fallback path starts.
- Template rendering still fails fast if placeholders remain unresolved.

## Review And Export

- After generation the user lands on a review dashboard instead of exiting immediately.
- The review dashboard shows:
  - generated Markdown artifact
  - resolved input summary
  - workflow validation checklist
  - resolved provider diagnostics
- Review actions:
  - `Tab` switches pane focus
  - `e` enters multiline edit mode
  - `s` opens the save-path prompt
  - `Esc` exits without saving

## Edit Mode

- Edit mode uses the shared multiline text field behavior already used elsewhere in the TUI.
- `Ctrl+S` accepts edits and returns to review.
- `Esc` discards the draft edit and restores the last accepted review content.
- Arrow keys, paging keys, and wrapped multiline navigation remain available while editing.

## Save Behavior

- Saving always uses a one-off path prompt.
- Default target:
  - `.metastack/workflows/generated/<workflow-name>.md`
  - When the workflow has a Linear issue parameter and a value is present, the default becomes `.metastack/workflows/generated/<workflow-name>-<issue>.md`
- Save safety:
  - output paths must stay inside the repository root
  - parent directories are created as needed
  - existing files are not replaced unless the user explicitly confirms overwrite in the TUI or passes `--overwrite` in non-interactive mode

## Changed Surfaces

- CLI contract:
  - `src/cli.rs`
- Workflow loading, routing, generation, TUI, and save behavior:
  - `src/workflows.rs`
- Command help coverage:
  - `tests/commands.rs`
- Workflow routing, snapshots, non-interactive save, and overwrite handling:
  - `tests/workflows.rs`
- User-facing command docs:
  - `README.md`
  - `src/artifacts/workflows/README.md`
