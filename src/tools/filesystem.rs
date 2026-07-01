use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

const SENSITIVE_COMPONENTS: &[&str] = &[".ssh", ".gnupg", ".aws"];
const SENSITIVE_FILES: &[&str] = &[
    ".zshrc",
    ".bashrc",
    ".profile",
    ".env",
    ".netrc",
    ".npmrc",
    ".git-credentials",
];

pub fn ensure_not_sensitive(path: &Path, home: &Path) -> Result<()> {
    check_sensitive_path(path, home)?;
    if let Some(resolved) = resolve_existing_ancestor(path) {
        let resolved_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        check_sensitive_path(&resolved, &resolved_home)?;
    }
    Ok(())
}

fn check_sensitive_path(path: &Path, home: &Path) -> Result<()> {
    let relative = path.strip_prefix(home).unwrap_or(path);
    let components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let sensitive_component = components
        .iter()
        .any(|name| SENSITIVE_COMPONENTS.contains(&name.as_str()));
    let sensitive_file = components
        .last()
        .is_some_and(|name| SENSITIVE_FILES.contains(&name.as_str()));
    let sensitive_library = components.starts_with(&["library".to_owned(), "keychains".to_owned()])
        || components.starts_with(&["library".to_owned(), "mail".to_owned()]);
    if sensitive_component || sensitive_file || sensitive_library {
        bail!(
            "access to protected credential/configuration path is blocked: {}",
            path.display()
        );
    }
    Ok(())
}

fn resolve_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::<OsString>::new();
    while !ancestor.exists() {
        suffix.push(ancestor.file_name()?.to_os_string());
        ancestor = ancestor.parent()?;
    }
    let mut resolved = std::fs::canonicalize(ancestor).ok()?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Some(resolved)
}

pub fn resolve_path(raw: &str, home: &Path) -> PathBuf {
    let path = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        let path = PathBuf::from(raw);
        if path.is_absolute() {
            path
        } else {
            home.join(path)
        }
    };
    normalize_lexically(&path)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

pub async fn path_status(path: &Path) -> Result<String> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => {
            let kind = if metadata.file_type().is_symlink() {
                "symlink"
            } else if metadata.is_dir() {
                "directory"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            Ok(format!(
                "exists: true\ntype: {kind}\npath: {}\nsize_bytes: {}\nmodified_unix: {modified}",
                path.display(),
                metadata.len()
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(format!("exists: false\npath: {}", path.display()))
        }
        Err(error) => Err(error).with_context(|| format!("cannot inspect {}", path.display())),
    }
}

pub async fn list_directory(path: &Path, limit: usize) -> Result<String> {
    let limit = limit.clamp(1, 500);
    let mut reader = tokio::fs::read_dir(path)
        .await
        .with_context(|| format!("cannot list {}", path.display()))?;
    let mut entries = Vec::new();
    while let Some(entry) = reader.next_entry().await? {
        let metadata = entry.metadata().await?;
        let kind = if metadata.is_dir() { "dir" } else { "file" };
        entries.push(format!(
            "{kind}\t{}\t{}",
            metadata.len(),
            entry.path().display()
        ));
    }
    entries.sort();
    entries.truncate(limit);
    Ok(if entries.is_empty() {
        "directory is empty".to_owned()
    } else {
        entries.join("\n")
    })
}

pub async fn find_files(
    root: &Path,
    query: &str,
    max_depth: usize,
    limit: usize,
) -> Result<String> {
    let root = root.to_path_buf();
    let query = query.to_ascii_lowercase();
    let max_depth = max_depth.clamp(1, 20);
    let limit = limit.clamp(1, 500);

    tokio::task::spawn_blocking(move || {
        if !root.is_dir() {
            bail!("search root is not a directory: {}", root.display());
        }
        let mut matches = Vec::new();
        for entry in WalkDir::new(&root)
            .max_depth(max_depth)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            if entry.depth() == 0 {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.contains(&query) {
                let kind = if entry.file_type().is_dir() {
                    "dir"
                } else {
                    "file"
                };
                matches.push(format!("{kind}\t{}", entry.path().display()));
                if matches.len() >= limit {
                    break;
                }
            }
        }
        Ok(if matches.is_empty() {
            "no matches".to_owned()
        } else {
            matches.join("\n")
        })
    })
    .await?
}

pub async fn read_file(path: &Path, max_bytes: usize) -> Result<String> {
    let max_bytes = max_bytes.clamp(1, 1_000_000);
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("cannot read {}", path.display()))?;
    let truncated = bytes.len() > max_bytes;
    // Back the cut off to a UTF-8 character boundary so lossy decoding does not
    // split a multi-byte sequence and emit replacement characters at the edge.
    let mut end = bytes.len().min(max_bytes);
    while end > 0 && end < bytes.len() && !is_utf8_char_boundary(bytes[end]) {
        end -= 1;
    }
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if truncated {
        text.push_str("\n[truncated]");
    }
    Ok(text)
}

/// True if `byte` is the start of a UTF-8 code point (or ASCII). Continuation
/// bytes have the top two bits set to `10`.
fn is_utf8_char_boundary(byte: u8) -> bool {
    (byte as i8) >= -0x40
}

pub async fn write_file(path: &Path, content: &str, overwrite: bool) -> Result<String> {
    if path.exists() && !overwrite {
        bail!(
            "a file already exists at {} and was not replaced because the task did not request overwriting it; if the user asked to replace it, retry with overwrite set to true, otherwise choose a different path",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, content)
        .await
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(format!(
        "status: complete\npath: {}\nbytes: {}",
        path.display(),
        content.len()
    ))
}

pub async fn create_directory(path: &Path) -> Result<String> {
    tokio::fs::create_dir_all(path)
        .await
        .with_context(|| format!("cannot create {}", path.display()))?;
    Ok(format!("status: complete\npath: {}", path.display()))
}

pub async fn move_to_trash(path: &Path, trash: &Path) -> Result<String> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .with_context(|| format!("cannot trash missing path {}", path.display()))?;
    let home = trash.parent().unwrap_or_else(|| Path::new("/"));
    let desktop = home.join("Desktop");
    if path == Path::new("/") || path == home || path == desktop {
        bail!("refusing to trash a protected root: {}", path.display());
    }

    tokio::fs::create_dir_all(trash).await?;
    let name = path
        .file_name()
        .context("cannot trash a path without a file name")?
        .to_string_lossy();
    let mut destination = trash.join(name.as_ref());
    let mut suffix = 1_u32;
    while destination.exists() {
        destination = trash.join(format!("{name} {suffix}"));
        suffix += 1;
    }

    tokio::fs::rename(path, &destination)
        .await
        .with_context(|| {
            format!(
                "cannot move {} to {}",
                path.display(),
                destination.display()
            )
        })?;
    let kind = if metadata.is_dir() {
        "directory"
    } else {
        "file"
    };
    Ok(format!(
        "status: complete\naction: moved {kind} to Trash\nfrom: {}\nto: {}",
        path.display(),
        destination.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_checks_reads_and_trashes_files() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let target = home.join("Desktop").join("Makis");
        let file = target.join("note.txt");

        create_directory(&target).await.unwrap();
        write_file(&file, "hello", false).await.unwrap();

        assert!(path_status(&target).await.unwrap().contains("exists: true"));
        assert_eq!(read_file(&file, 100).await.unwrap(), "hello");

        let result = move_to_trash(&target, &home.join(".Trash")).await.unwrap();
        assert!(result.contains("status: complete"));
        assert!(!target.exists());
        assert!(home.join(".Trash").join("Makis").exists());
    }

    #[tokio::test]
    async fn write_file_reports_actionable_error_for_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("note.txt");
        write_file(&file, "first", false).await.unwrap();

        let error = write_file(&file, "second", false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"));
        assert!(error.contains("overwrite set to true"));
        // The original content must be untouched by a refused write.
        assert_eq!(read_file(&file, 100).await.unwrap(), "first");

        // With overwrite the write proceeds.
        write_file(&file, "second", true).await.unwrap();
        assert_eq!(read_file(&file, 100).await.unwrap(), "second");
    }

    #[tokio::test]
    async fn finds_named_items() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("Reports");
        tokio::fs::create_dir_all(&target).await.unwrap();
        tokio::fs::write(target.join("invoice-june.pdf"), b"pdf")
            .await
            .unwrap();

        let result = find_files(temp.path(), "invoice", 5, 10).await.unwrap();
        assert!(result.contains("invoice-june.pdf"));
    }

    #[tokio::test]
    async fn protects_desktop_root_from_trash() {
        let temp = tempfile::tempdir().unwrap();
        let desktop = temp.path().join("Desktop");
        tokio::fs::create_dir_all(&desktop).await.unwrap();
        assert!(
            move_to_trash(&desktop, &temp.path().join(".Trash"))
                .await
                .is_err()
        );
    }

    #[test]
    fn normalizes_parent_components_before_execution() {
        let home = Path::new("/Users/tester");
        assert_eq!(resolve_path("~/Desktop/..", home), home);
        assert_eq!(
            resolve_path("~/Desktop/Makis/../Other", home),
            home.join("Desktop/Other")
        );
    }

    #[test]
    fn identifies_sensitive_user_paths() {
        let home = Path::new("/Users/tester");
        assert!(ensure_not_sensitive(&home.join(".ssh/id_ed25519"), home).is_err());
        assert!(
            ensure_not_sensitive(&home.join("Library/Keychains/login.keychain-db"), home).is_err()
        );
        assert!(ensure_not_sensitive(&home.join("Documents/report.txt"), home).is_ok());
    }

    #[tokio::test]
    async fn truncates_on_a_utf8_char_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("multibyte.txt");
        // "é" is two bytes (0xC3 0xA9); cutting at 1 byte would split it.
        tokio::fs::write(&file, "aé".as_bytes()).await.unwrap();

        let result = read_file(&file, 2).await.unwrap();
        // The cut backs off to before "é", so no replacement char appears.
        assert!(result.starts_with('a'));
        assert!(!result.contains('\u{fffd}'));
        assert!(result.contains("[truncated]"));

        // Reading with enough budget returns the whole string intact.
        assert!(read_file(&file, 100).await.unwrap().starts_with("aé"));
    }

    #[test]
    fn identifies_utf8_char_boundaries() {
        assert!(is_utf8_char_boundary(b'a'));
        assert!(is_utf8_char_boundary(0xC3)); // leading byte
        assert!(!is_utf8_char_boundary(0xA9)); // continuation byte
    }
}
