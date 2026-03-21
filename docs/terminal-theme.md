# Shared Terminal Theme Notes

## Gradient-capable styling options

- `ratatui` RGB spans fit this codebase best when a ratatui dashboard needs a small, static accent, because the color data stays inside widget rendering and snapshot tests remain deterministic.
- `colorgrad` is useful only if we decide to generate true multi-stop ramps. The current dashboards are data-dense, so semantic colors are more valuable than decorative gradients.
- `owo-colors` works well for one-shot stdout and stderr text, but it does not integrate naturally with ratatui widget trees.
- `nu-ansi-term` has a similar stdout-first tradeoff and adds little value once a screen is already composed from ratatui spans.

## Decision

Use a shared semantic palette for MetaStack dashboards and loading flows. Gradients are appropriate only for optional ratatui-only hero or loading accents, not for list-heavy dashboards, tables, or status copy.

## Fallback boundary

When richer color output is unavailable, the shared theme falls back to a single accent color instead of approximating gradients with ANSI palettes. The fallback boundary is `src/tui/theme.rs`, which checks terminal hints such as `NO_COLOR`, `TERM=dumb`, and `COLORTERM=truecolor` before selecting accent colors.

Planning and technical backlog loading screens stay inside this same boundary because `src/plan.rs` and `src/technical.rs` both render their waiting state through `src/progress.rs::render_loading_panel`. That keeps long-running dashboard flows on one shared treatment instead of introducing command-specific loading styles.
