use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

pub async fn pasted_image_path(input: &str) -> Option<PathBuf> {
    let path = normalize_pasted_path(input);
    image_mime_type(&path)?;
    tokio::fs::metadata(&path)
        .await
        .ok()
        .filter(|metadata| metadata.is_file())
        .map(|_| path)
}

pub async fn image_data_url(path: &Path) -> Result<String> {
    let mime_type = image_mime_type(path).with_context(|| {
        format!(
            "unsupported image type for {}; use PNG, JPEG, WEBP, or GIF",
            path.display()
        )
    })?;
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("cannot inspect image {}", path.display()))?;
    if metadata.len() > MAX_IMAGE_BYTES {
        bail!(
            "image is too large ({} bytes); maximum is {MAX_IMAGE_BYTES} bytes",
            metadata.len()
        );
    }
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("cannot read image {}", path.display()))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime_type};base64,{encoded}"))
}

fn normalize_pasted_path(input: &str) -> PathBuf {
    let trimmed = input.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            trimmed
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(trimmed);
    PathBuf::from(unquoted.replace("\\ ", " "))
}

fn image_mime_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_quoted_and_escaped_paths() {
        assert_eq!(
            normalize_pasted_path(r#""/tmp/an image.png""#),
            PathBuf::from("/tmp/an image.png")
        );
        assert_eq!(
            normalize_pasted_path(r"/tmp/an\ image.png"),
            PathBuf::from("/tmp/an image.png")
        );
    }

    #[test]
    fn recognizes_supported_image_types() {
        assert_eq!(image_mime_type(Path::new("photo.PNG")), Some("image/png"));
        assert_eq!(image_mime_type(Path::new("photo.heic")), None);
    }

    #[tokio::test]
    async fn encodes_local_image_as_data_url() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        tokio::fs::write(&path, b"png").await.unwrap();
        assert_eq!(
            image_data_url(&path).await.unwrap(),
            "data:image/png;base64,cG5n"
        );
    }
}
