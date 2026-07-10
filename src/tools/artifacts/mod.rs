mod documents;
mod images;
mod pdf;
mod spreadsheet;

use std::fs::File;
use std::io::Read;
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
const MAX_XML_TAG_BYTES: usize = 64 * 1024;
const MAX_XML_ATTRIBUTES: usize = 256;

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
        let mut entry = archive.by_index(index)?;
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
        let name = entry.name().to_ascii_lowercase();
        if name.ends_with(".xml") || name.ends_with(".rels") {
            scan_xml_structure(&mut entry, &name)?;
        }
    }
    Ok(())
}

/// Rejects XML constructs that can trigger pathological behavior in parser
/// versions still used by the latest docx-rs and umya-spreadsheet releases.
/// This scan is linear and runs before either dependency sees untrusted OOXML.
fn scan_xml_structure(reader: &mut impl Read, name: &str) -> Result<()> {
    let mut buffer = [0_u8; 8192];
    let mut in_tag = false;
    let mut quote = None;
    let mut tag_bytes = 0_usize;
    let mut attributes = 0_usize;

    loop {
        let count = reader
            .read(&mut buffer)
            .with_context(|| format!("cannot inspect XML archive entry {name}"))?;
        if count == 0 {
            break;
        }
        for &byte in &buffer[..count] {
            if !in_tag {
                if byte == b'<' {
                    in_tag = true;
                    quote = None;
                    tag_bytes = 1;
                    attributes = 0;
                }
                continue;
            }

            tag_bytes += 1;
            if tag_bytes > MAX_XML_TAG_BYTES {
                bail!(
                    "XML archive entry {name} contains a tag larger than {MAX_XML_TAG_BYTES} bytes"
                );
            }
            if let Some(delimiter) = quote {
                if byte == delimiter {
                    quote = None;
                }
                continue;
            }
            match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'=' => {
                    attributes += 1;
                    if attributes > MAX_XML_ATTRIBUTES {
                        bail!(
                            "XML archive entry {name} contains more than {MAX_XML_ATTRIBUTES} attributes in one tag"
                        );
                    }
                }
                b'>' => in_tag = false,
                _ => {}
            }
        }
    }
    if in_tag {
        bail!("XML archive entry {name} ends inside an unterminated tag");
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

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Cursor;
    use std::io::Write;

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::*;

    #[test]
    fn rejects_xml_tags_with_excessive_attributes() {
        let attributes = (0..=MAX_XML_ATTRIBUTES)
            .map(|index| format!(" a{index}=\"x\""))
            .collect::<String>();
        let xml = format!("<root{attributes}/>");
        let error = scan_xml_structure(&mut Cursor::new(xml), "word/document.xml")
            .expect_err("attribute limit must be enforced");
        assert!(error.to_string().contains("more than 256 attributes"));
    }

    #[test]
    fn accepts_normal_xml_and_quoted_delimiters() {
        let xml = br#"<root value="a > b = c"><child id='1'>ok</child></root>"#;
        scan_xml_structure(&mut Cursor::new(xml), "word/document.xml").unwrap();
    }

    #[test]
    fn archive_preflight_rejects_hostile_ooxml_before_parsing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("hostile.xlsx");
        let file = File::create(&path).unwrap();
        let mut archive = ZipWriter::new(file);
        archive
            .start_file("xl/workbook.xml", SimpleFileOptions::default())
            .unwrap();
        let attributes = (0..=MAX_XML_ATTRIBUTES)
            .map(|index| format!(" a{index}=\"x\""))
            .collect::<String>();
        archive
            .write_all(format!("<workbook{attributes}/>").as_bytes())
            .unwrap();
        archive.finish().unwrap();

        let error = ensure_safe_archive(&path).expect_err("hostile OOXML must be rejected");
        assert!(error.to_string().contains("more than 256 attributes"));
    }
}
