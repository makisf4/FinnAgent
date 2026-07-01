use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, Limits};

use super::{commit_output, ensure_input_file, extension, prepare_output};

pub fn inspect_image(path: &Path) -> Result<String> {
    ensure_input_file(path)?;
    let reader = limited_reader(path)?;
    let format = reader
        .format()
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| "unknown".to_owned());
    let image = reader.decode()?;
    Ok(format!(
        "type: image\npath: {}\nformat: {format}\nwidth: {}\nheight: {}\ncolor: {:?}",
        path.display(),
        image.width(),
        image.height(),
        image.color()
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn transform_image(
    input: &Path,
    output: &Path,
    operation: &str,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    degrees: u32,
    overwrite: bool,
) -> Result<String> {
    ensure_input_file(input)?;
    let output_format = image_format(output)?;
    let temporary = prepare_output(output, overwrite || input == output)?;
    let mut image = decode_limited(input)?;
    image = match operation {
        "convert" => image,
        "resize" => {
            if width == 0 || height == 0 {
                bail!("resize width and height must be greater than zero");
            }
            image.resize_exact(width, height, FilterType::Lanczos3)
        }
        "crop" => {
            if width == 0
                || height == 0
                || x.saturating_add(width) > image.width()
                || y.saturating_add(height) > image.height()
            {
                bail!("crop rectangle is empty or outside the image bounds");
            }
            image.crop_imm(x, y, width, height)
        }
        "rotate" => match degrees {
            90 => image.rotate90(),
            180 => image.rotate180(),
            270 => image.rotate270(),
            _ => bail!("rotate degrees must be 90, 180, or 270"),
        },
        "flip_horizontal" => image.fliph(),
        "flip_vertical" => image.flipv(),
        "grayscale" => image.grayscale(),
        _ => bail!(
            "unsupported image operation '{operation}'; expected convert, resize, crop, rotate, flip_horizontal, flip_vertical, or grayscale"
        ),
    };
    let file = File::create(&temporary)?;
    let mut writer = BufWriter::new(file);
    if let Err(error) = image.write_to(&mut writer, output_format) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).context("cannot encode transformed image");
    }
    drop(writer);
    commit_output(&temporary, output)?;
    Ok(format!(
        "status: complete\npath: {}\nwidth: {}\nheight: {}\nformat: {}",
        output.display(),
        image.width(),
        image.height(),
        extension(output)?
    ))
}

fn limited_reader(path: &Path) -> Result<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    let mut reader = image::ImageReader::open(path)
        .with_context(|| format!("cannot open image {}", path.display()))?
        .with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(50_000);
    limits.max_image_height = Some(50_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader)
}

fn decode_limited(path: &Path) -> Result<DynamicImage> {
    limited_reader(path)?
        .decode()
        .with_context(|| format!("cannot decode image {}", path.display()))
}

fn image_format(path: &Path) -> Result<ImageFormat> {
    match extension(path)?.as_str() {
        "png" => Ok(ImageFormat::Png),
        "jpg" | "jpeg" => Ok(ImageFormat::Jpeg),
        "gif" => Ok(ImageFormat::Gif),
        "webp" => Ok(ImageFormat::WebP),
        "tif" | "tiff" => Ok(ImageFormat::Tiff),
        other => bail!("unsupported output image extension '.{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use image::{GenericImageView, ImageBuffer, Rgb};

    use super::*;

    #[test]
    fn inspects_resizes_rotates_and_converts_images() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.png");
        let resized = temp.path().join("resized.jpg");
        let rotated = temp.path().join("rotated.png");
        let source = ImageBuffer::from_pixel(8, 4, Rgb([20_u8, 40, 60]));
        source.save(&input).unwrap();

        let inspection = inspect_image(&input).unwrap();
        assert!(inspection.contains("width: 8"));
        assert!(inspection.contains("height: 4"));

        transform_image(&input, &resized, "resize", 0, 0, 4, 2, 0, false).unwrap();
        assert_eq!(image::open(&resized).unwrap().dimensions(), (4, 2));

        transform_image(&resized, &rotated, "rotate", 0, 0, 0, 0, 90, false).unwrap();
        assert_eq!(image::open(&rotated).unwrap().dimensions(), (2, 4));
    }
}
