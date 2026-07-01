mod documents;
mod images;
mod pdf;
mod spreadsheet;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use zip::ZipArchive;

pub use documents::{create_document, read_artifact, replace_document_text};
pub use images::transform_image;
pub use pdf::{replace_pdf_text, transform_pdf_pages};
pub use spreadsheet::update_spreadsheet;

const MAX_ARTIFACT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 100 * 1024 * 1024;
const MAX_ARCHIVE_EXPANDED_BYTES: u64 = 250 * 1024 * 1024;

fn ensure_input_file(path: &Path) -> Result<()> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("artifact does not exist: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("artifact is not a file: {}", path.display());
    }
    if metadata.len() > MAX_ARTIFACT_BYTES {
        bail!(
            "artifact exceeds the {} MB processing limit: {}",
            MAX_ARTIFACT_BYTES / (1024 * 1024),
            path.display()
        );
    }
    if matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("docx" | "xlsx")
    ) {
        ensure_safe_archive(path)?;
    }
    Ok(())
}

fn ensure_safe_archive(path: &Path) -> Result<()> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("invalid ZIP package: {}", path.display()))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        bail!(
            "artifact contains too many archive entries ({} > {MAX_ARCHIVE_ENTRIES})",
            archive.len()
        );
    }
    let mut expanded = 0_u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.size() > MAX_ARCHIVE_ENTRY_BYTES {
            bail!(
                "artifact archive entry is too large ({} bytes): {}",
                entry.size(),
                entry.name()
            );
        }
        expanded = expanded
            .checked_add(entry.size())
            .context("artifact expanded size overflow")?;
        if expanded > MAX_ARCHIVE_EXPANDED_BYTES {
            bail!(
                "artifact expands beyond the {} MB safety limit",
                MAX_ARCHIVE_EXPANDED_BYTES / (1024 * 1024)
            );
        }
    }
    Ok(())
}

fn prepare_output(path: &Path, overwrite: bool) -> Result<PathBuf> {
    let parent = path
        .parent()
        .context("output path must have a parent directory")?;
    let metadata = std::fs::metadata(parent)
        .with_context(|| format!("output directory does not exist: {}", parent.display()))?;
    if !metadata.is_dir() {
        bail!("output parent is not a directory: {}", parent.display());
    }
    if path.exists() && !overwrite {
        bail!(
            "a file already exists at {} and was not replaced because the task did not request overwriting it; if the user asked to replace it, retry with overwrite set to true, otherwise choose a different path",
            path.display()
        );
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("output path must have a valid file name")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(".{name}.finn-{}-{nonce}.tmp", std::process::id())))
}

fn commit_output(temporary: &Path, destination: &Path) -> Result<()> {
    if let Err(error) = std::fs::rename(temporary, destination) {
        let _ = std::fs::remove_file(temporary);
        return Err(error).with_context(|| {
            format!(
                "cannot move completed artifact from {} to {}",
                temporary.display(),
                destination.display()
            )
        });
    }
    Ok(())
}

fn extension(path: &Path) -> Result<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .with_context(|| format!("file has no usable extension: {}", path.display()))
}

fn clipped(mut value: String, max_chars: usize) -> String {
    let max_chars = max_chars.clamp(1, 1_000_000);
    if value.chars().count() <= max_chars {
        return value;
    }
    value = value.chars().take(max_chars).collect();
    value.push_str("\n[truncated]");
    value
}
