use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use image::ImageReader;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat};
use reqwest::Url;

pub(crate) const MAX_PROMPT_IMAGES: usize = 5;
const MAX_PROVIDER_IMAGE_WIDTH: u32 = 2048;
const MAX_PROVIDER_IMAGE_HEIGHT: u32 = 768;
const WINDOWS_CLIPBOARD_PASTE_UNSUPPORTED_MESSAGE: &str = "clipboard image paste is not supported on Windows yet; paste a readable local image path or file:// URL instead";
const WSL_CLIPBOARD_PASTE_UNSUPPORTED_MESSAGE: &str = "clipboard image paste is not supported on WSL yet; paste a readable local image path or file:// URL instead";
const GENERIC_CLIPBOARD_PASTE_UNSUPPORTED_MESSAGE: &str = "clipboard image paste is not supported on this platform yet; paste a readable local image path or file:// URL instead";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptImageAttachment {
    pub(crate) original_path: PathBuf,
    pub(crate) stored_path: PathBuf,
    pub(crate) display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EncodedPromptImage {
    pub(crate) display_name: String,
    pub(crate) base64_png: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) resized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClipboardPromptPaste {
    Text(String),
    Attachment(PromptImageAttachment),
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardPastePlatform {
    MacOs,
    Linux,
    Windows,
    Wsl,
    Unsupported,
}

pub(crate) fn resolve_attachment_from_pasted_text(
    text: &str,
) -> Result<Option<PromptImageAttachment>> {
    let Some(path) = pasted_image_path(text)? else {
        return Ok(None);
    };
    store_prompt_image_from_pasted_path(&path)
}

pub(crate) fn resolve_clipboard_prompt_paste() -> Result<ClipboardPromptPaste> {
    match clipboard_paste_platform(running_in_wsl()?) {
        ClipboardPastePlatform::MacOs => resolve_macos_clipboard_prompt_paste(),
        ClipboardPastePlatform::Linux => resolve_linux_clipboard_prompt_paste(),
        unsupported => {
            if let Some(message) = clipboard_platform_unsupported_message(unsupported) {
                bail!("{message}");
            }
            unreachable!("supported clipboard platforms should return before this branch");
        }
    }
}

pub(crate) fn encode_prompt_images_for_provider(
    attachments: &[PromptImageAttachment],
) -> Result<Vec<EncodedPromptImage>> {
    attachments
        .iter()
        .map(|attachment| {
            let image = image::open(&attachment.stored_path).with_context(|| {
                format!(
                    "failed to decode prompt image `{}`",
                    attachment.stored_path.display()
                )
            })?;
            let (original_width, original_height) = image.dimensions();
            let resized_image = if original_width > MAX_PROVIDER_IMAGE_WIDTH
                || original_height > MAX_PROVIDER_IMAGE_HEIGHT
            {
                image.resize(
                    MAX_PROVIDER_IMAGE_WIDTH,
                    MAX_PROVIDER_IMAGE_HEIGHT,
                    FilterType::Lanczos3,
                )
            } else {
                image
            };
            let (width, height) = resized_image.dimensions();
            let resized = width != original_width || height != original_height;
            let mut bytes = Vec::new();
            resized_image
                .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
                .context("failed to encode prompt image as PNG")?;

            Ok(EncodedPromptImage {
                display_name: attachment.display_name.clone(),
                base64_png: BASE64_STANDARD.encode(bytes),
                width,
                height,
                resized,
            })
        })
        .collect()
}

fn resolve_macos_clipboard_prompt_paste() -> Result<ClipboardPromptPaste> {
    if let Some(attachment) = read_macos_clipboard_image()? {
        return Ok(ClipboardPromptPaste::Attachment(attachment));
    }

    let text = command_stdout(&mut Command::new("pbpaste"))?;
    if text.is_empty() {
        Ok(ClipboardPromptPaste::Empty)
    } else {
        Ok(ClipboardPromptPaste::Text(text))
    }
}

fn resolve_linux_clipboard_prompt_paste() -> Result<ClipboardPromptPaste> {
    if let Some(bytes) = read_linux_clipboard_image()? {
        return Ok(ClipboardPromptPaste::Attachment(store_prompt_image_bytes(
            bytes,
            "clipboard.png",
        )?));
    }

    if let Some(text) = read_linux_clipboard_text()? {
        if text.is_empty() {
            return Ok(ClipboardPromptPaste::Empty);
        }
        return Ok(ClipboardPromptPaste::Text(text));
    }

    bail!(
        "clipboard paste requires `wl-paste` on Wayland or `xclip` on X11; paste text directly through the terminal or use an image path/file:// URL instead"
    )
}

fn read_macos_clipboard_image() -> Result<Option<PromptImageAttachment>> {
    let destination = next_session_file_path("clipboard", "png");
    let destination_literal = applescript_string_literal(&destination.display().to_string());
    let script = format!(
        "try\n\
set outputPath to POSIX file {destination_literal}\n\
set imageData to the clipboard as «class PNGf»\n\
set fileHandle to open for access outputPath with write permission\n\
set eof fileHandle to 0\n\
write imageData to fileHandle\n\
close access fileHandle\n\
on error errMsg number errNum\n\
try\n\
close access POSIX file {destination_literal}\n\
end try\n\
if errNum is -1700 or errNum is -1715 then\n\
return \"__NO_IMAGE__\"\n\
end if\n\
error errMsg number errNum\n\
end try"
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .context("failed to invoke `osascript` for clipboard image paste")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-1700") || stderr.contains("-1715") {
            return Ok(None);
        }
        bail!(
            "failed to read a clipboard image on macOS: {}",
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("__NO_IMAGE__") || !destination.exists() {
        return Ok(None);
    }

    Ok(Some(store_prompt_image(&destination)?))
}

fn read_linux_clipboard_image() -> Result<Option<Vec<u8>>> {
    if env::var_os("WAYLAND_DISPLAY").is_some() && command_exists("wl-paste") {
        let output = Command::new("wl-paste")
            .arg("-t")
            .arg("image/png")
            .output()
            .context("failed to invoke `wl-paste` for clipboard image paste")?;
        if output.status.success() && !output.stdout.is_empty() {
            return Ok(Some(output.stdout));
        }
    }

    if env::var_os("DISPLAY").is_some() && command_exists("xclip") {
        let output = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "image/png", "-o"])
            .output()
            .context("failed to invoke `xclip` for clipboard image paste")?;
        if output.status.success() && !output.stdout.is_empty() {
            return Ok(Some(output.stdout));
        }
    }

    Ok(None)
}

fn read_linux_clipboard_text() -> Result<Option<String>> {
    if env::var_os("WAYLAND_DISPLAY").is_some() && command_exists("wl-paste") {
        let output = Command::new("wl-paste")
            .arg("--no-newline")
            .output()
            .context("failed to invoke `wl-paste` for clipboard text paste")?;
        if output.status.success() {
            return Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()));
        }
    }

    if env::var_os("DISPLAY").is_some() && command_exists("xclip") {
        let output = Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
            .context("failed to invoke `xclip` for clipboard text paste")?;
        if output.status.success() {
            return Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()));
        }
    }

    Ok(None)
}

fn pasted_image_path(text: &str) -> Result<Option<PathBuf>> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return Ok(None);
    }

    if let Some(url) = trimmed.strip_prefix("file://") {
        let parsed = Url::parse(&format!("file://{url}"))
            .with_context(|| format!("failed to parse pasted file URL `{trimmed}`"))?;
        let file_path = parsed
            .to_file_path()
            .map_err(|_| anyhow!("pasted file URL `{trimmed}` could not be resolved locally"))?;
        return Ok(Some(file_path));
    }

    let candidate = PathBuf::from(trimmed);
    if candidate.exists() {
        return Ok(Some(candidate));
    }

    Ok(None)
}

fn store_prompt_image(path: &Path) -> Result<PromptImageAttachment> {
    let image = image::open(path)
        .with_context(|| format!("failed to read image file `{}`", path.display()))?;
    let display_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| "image.png".to_string());
    let stored_path = next_session_file_path("attachment", "png");
    write_png(&image, &stored_path)?;
    Ok(PromptImageAttachment {
        original_path: path.to_path_buf(),
        stored_path,
        display_name,
    })
}

fn store_prompt_image_from_pasted_path(path: &Path) -> Result<Option<PromptImageAttachment>> {
    let reader = ImageReader::open(path)
        .with_context(|| format!("failed to read image file `{}`", path.display()))?;
    let reader = reader
        .with_guessed_format()
        .with_context(|| format!("failed to inspect pasted file `{}`", path.display()))?;
    if reader.format().is_none() {
        return Ok(None);
    }

    Ok(Some(store_prompt_image(path)?))
}

fn store_prompt_image_bytes(bytes: Vec<u8>, display_name: &str) -> Result<PromptImageAttachment> {
    let image =
        image::load_from_memory(&bytes).context("failed to decode pasted clipboard image")?;
    let stored_path = next_session_file_path("attachment", "png");
    write_png(&image, &stored_path)?;
    Ok(PromptImageAttachment {
        original_path: stored_path.clone(),
        stored_path,
        display_name: display_name.to_string(),
    })
}

fn write_png(image: &DynamicImage, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    image
        .save_with_format(destination, ImageFormat::Png)
        .with_context(|| format!("failed to write `{}`", destination.display()))
}

fn next_session_file_path(prefix: &str, extension: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let session_root = session_root();
    let index = COUNTER.fetch_add(1, Ordering::Relaxed);
    session_root.join(format!("{prefix}-{index}.{extension}"))
}

fn session_root() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let session_stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        env::temp_dir()
            .join("metastack-prompt-images")
            .join(format!("{}-{session_stamp}", std::process::id()))
    })
}

fn command_exists(name: &str) -> bool {
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|path| path.join(name).exists()))
        .unwrap_or(false)
}

fn command_stdout(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .context("failed to run clipboard command")?;
    if !output.status.success() {
        bail!(
            "clipboard command exited unsuccessfully: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn applescript_string_literal(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn running_in_wsl() -> Result<bool> {
    if !cfg!(target_os = "linux") {
        return Ok(false);
    }

    let contents = fs::read_to_string("/proc/sys/kernel/osrelease")
        .or_else(|_| fs::read_to_string("/proc/version"))
        .unwrap_or_default();
    Ok(contents.to_ascii_lowercase().contains("microsoft"))
}

fn clipboard_paste_platform(in_wsl: bool) -> ClipboardPastePlatform {
    if cfg!(target_os = "macos") {
        return ClipboardPastePlatform::MacOs;
    }

    if cfg!(windows) {
        return ClipboardPastePlatform::Windows;
    }

    if cfg!(target_os = "linux") {
        if in_wsl {
            return ClipboardPastePlatform::Wsl;
        }
        return ClipboardPastePlatform::Linux;
    }

    ClipboardPastePlatform::Unsupported
}

fn clipboard_platform_unsupported_message(
    platform: ClipboardPastePlatform,
) -> Option<&'static str> {
    match platform {
        ClipboardPastePlatform::Windows => Some(WINDOWS_CLIPBOARD_PASTE_UNSUPPORTED_MESSAGE),
        ClipboardPastePlatform::Wsl => Some(WSL_CLIPBOARD_PASTE_UNSUPPORTED_MESSAGE),
        ClipboardPastePlatform::Unsupported => Some(GENERIC_CLIPBOARD_PASTE_UNSUPPORTED_MESSAGE),
        ClipboardPastePlatform::MacOs | ClipboardPastePlatform::Linux => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClipboardPastePlatform, EncodedPromptImage, MAX_PROVIDER_IMAGE_HEIGHT,
        MAX_PROVIDER_IMAGE_WIDTH, clipboard_paste_platform, clipboard_platform_unsupported_message,
        encode_prompt_images_for_provider, resolve_attachment_from_pasted_text,
    };
    use crate::tui::prompt_images::PromptImageAttachment;
    use anyhow::Result;
    use image::{ImageBuffer, Rgba};
    use reqwest::Url;
    use tempfile::{NamedTempFile, tempdir};

    #[test]
    fn oversized_images_are_resized_before_base64_encoding() -> Result<()> {
        let temp = tempdir()?;
        let image_path = temp.path().join("wide.png");
        let image = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(4096, 1024, Rgba([1, 2, 3, 255]));
        image.save(&image_path)?;

        let encoded = encode_prompt_images_for_provider(&[PromptImageAttachment {
            original_path: image_path.clone(),
            stored_path: image_path,
            display_name: "wide.png".to_string(),
        }])?;

        let EncodedPromptImage {
            width,
            height,
            resized,
            ..
        } = &encoded[0];
        assert!(resized);
        assert!(*width <= MAX_PROVIDER_IMAGE_WIDTH);
        assert!(*height <= MAX_PROVIDER_IMAGE_HEIGHT);

        Ok(())
    }

    #[test]
    fn file_url_paste_resolves_to_attachment() -> Result<()> {
        let temp = tempdir()?;
        let image_path = temp.path().join("proof.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&image_path)?;
        let url = Url::from_file_path(&image_path).expect("file url");

        let attachment = resolve_attachment_from_pasted_text(url.as_str())?.expect("attachment");

        assert_eq!(attachment.original_path, image_path);
        assert_eq!(attachment.display_name, "proof.png");
        assert!(attachment.stored_path.exists());
        assert_ne!(attachment.stored_path, attachment.original_path);
        Ok(())
    }

    #[test]
    fn non_image_paths_fall_back_to_text_paste() -> Result<()> {
        let file = NamedTempFile::new()?;
        std::fs::write(file.path(), "not an image")?;

        let attachment = resolve_attachment_from_pasted_text(&file.path().display().to_string())?;

        assert_eq!(attachment, None);
        Ok(())
    }

    #[test]
    fn clipboard_platform_prefers_wsl_over_linux() {
        #[cfg(target_os = "linux")]
        assert_eq!(clipboard_paste_platform(true), ClipboardPastePlatform::Wsl);

        #[cfg(not(target_os = "linux"))]
        assert_eq!(
            clipboard_paste_platform(true),
            clipboard_paste_platform(false)
        );
    }

    #[test]
    fn clipboard_platform_matches_current_host_when_not_in_wsl() {
        #[cfg(target_os = "macos")]
        assert_eq!(
            clipboard_paste_platform(false),
            ClipboardPastePlatform::MacOs
        );
        #[cfg(all(target_os = "linux", not(windows)))]
        assert_eq!(
            clipboard_paste_platform(false),
            ClipboardPastePlatform::Linux
        );
        #[cfg(windows)]
        assert_eq!(
            clipboard_paste_platform(false),
            ClipboardPastePlatform::Windows
        );
    }

    #[test]
    fn unsupported_clipboard_platforms_report_clear_messages() {
        assert_eq!(
            clipboard_platform_unsupported_message(ClipboardPastePlatform::Windows),
            Some(
                "clipboard image paste is not supported on Windows yet; paste a readable local image path or file:// URL instead"
            )
        );
        assert_eq!(
            clipboard_platform_unsupported_message(ClipboardPastePlatform::Wsl),
            Some(
                "clipboard image paste is not supported on WSL yet; paste a readable local image path or file:// URL instead"
            )
        );
        assert_eq!(
            clipboard_platform_unsupported_message(ClipboardPastePlatform::Unsupported),
            Some(
                "clipboard image paste is not supported on this platform yet; paste a readable local image path or file:// URL instead"
            )
        );
        assert_eq!(
            clipboard_platform_unsupported_message(ClipboardPastePlatform::MacOs),
            None
        );
        assert_eq!(
            clipboard_platform_unsupported_message(ClipboardPastePlatform::Linux),
            None
        );
    }
}
