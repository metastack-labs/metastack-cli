//! Build-time branding constants read from `[package.metadata.branding]` in `Cargo.toml`.
//!
//! Forking teams can white-label the CLI by editing the branding section and rebuilding.
//! The `[[bin]] name` and `default-run` fields should be updated alongside these values.

/// The CLI command name shown in help text, examples, and error messages.
pub const COMMAND_NAME: &str = env!("BRAND_COMMAND_NAME");

/// The project directory name used for local state (e.g. `.metastack`).
pub const PROJECT_DIR: &str = env!("BRAND_PROJECT_DIR");

/// The human-readable product name used in display text.
pub const DISPLAY_NAME: &str = env!("BRAND_DISPLAY_NAME");
