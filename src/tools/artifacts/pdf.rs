use std::path::Path;

use anyhow::{Context, Result, bail};
use lopdf::{Document, Object};

use super::{commit_output, ensure_input_file, extension, prepare_output};

pub fn read_pdf(path: &Path) -> Result<String> {
    let document =
        Document::load(path).with_context(|| format!("cannot parse PDF {}", path.display()))?;
    let pages = document.get_pages();
    let page_numbers = pages.keys().copied().collect::<Vec<_>>();
    let text = document
        .extract_text(&page_numbers)
        .context("cannot extract text from PDF")?;
    Ok(format!(
        "type: pdf\npath: {}\npages: {}\n\n{text}",
        path.display(),
        pages.len()
    ))
}

pub fn replace_pdf_text(
    input: &Path,
    output: &Path,
    page_number: u32,
    find: &str,
    replacement: &str,
    overwrite: bool,
) -> Result<String> {
    validate_pdf_paths(input, output)?;
    if find.is_empty() {
        bail!("find text must not be empty");
    }
    let temporary = prepare_output(output, overwrite || input == output)?;
    let mut document = Document::load(input)?;
    let page_count = document.get_pages().len() as u32;
    let pages = if page_number == 0 {
        (1..=page_count).collect::<Vec<_>>()
    } else {
        vec![page_number]
    };
    let mut replacements = 0_usize;
    for page in pages {
        replacements += document
            .replace_partial_text(page, find, replacement, None)
            .with_context(|| format!("cannot replace text on PDF page {page}"))?;
    }
    if replacements == 0 {
        bail!("text was not found in replaceable PDF text operations");
    }
    if let Err(error) = document.save(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).context("cannot save modified PDF");
    }
    commit_output(&temporary, output)?;
    Ok(format!(
        "status: complete\npath: {}\nreplacements: {replacements}",
        output.display()
    ))
}

pub fn transform_pdf_pages(
    input: &Path,
    output: &Path,
    operation: &str,
    page_numbers: &[u32],
    degrees: i64,
    overwrite: bool,
) -> Result<String> {
    validate_pdf_paths(input, output)?;
    if page_numbers.is_empty() {
        bail!("page_numbers must not be empty");
    }
    let temporary = prepare_output(output, overwrite || input == output)?;
    let mut document = Document::load(input)?;
    let pages = document.get_pages();
    for page in page_numbers {
        if !pages.contains_key(page) {
            bail!("PDF page {page} does not exist");
        }
    }
    match operation {
        "remove" => {
            if page_numbers.len() >= pages.len() {
                bail!("refusing to remove every page from the PDF");
            }
            document.delete_pages(page_numbers);
            document.prune_objects();
        }
        "rotate" => {
            if !matches!(degrees, 90 | 180 | 270 | -90 | -180 | -270) {
                bail!("rotation degrees must be 90, 180, 270, -90, -180, or -270");
            }
            for page_number in page_numbers {
                let page_id = pages[page_number];
                let page = document.get_object_mut(page_id)?.as_dict_mut()?;
                let existing = page
                    .get(b"Rotate")
                    .ok()
                    .and_then(|value| value.as_i64().ok())
                    .unwrap_or(0);
                let normalized = (existing + degrees).rem_euclid(360);
                page.set("Rotate", Object::Integer(normalized));
            }
        }
        _ => bail!("unsupported PDF operation '{operation}'; expected remove or rotate"),
    }
    if let Err(error) = document.save(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).context("cannot save transformed PDF");
    }
    commit_output(&temporary, output)?;
    Ok(format!(
        "status: complete\npath: {}\noperation: {operation}\npages: {:?}",
        output.display(),
        page_numbers
    ))
}

fn validate_pdf_paths(input: &Path, output: &Path) -> Result<()> {
    ensure_input_file(input)?;
    if extension(input)? != "pdf" || extension(output)? != "pdf" {
        bail!("PDF operations require .pdf input and output paths");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use lopdf::content::{Content, Operation};
    use lopdf::{Stream, dictionary};

    use super::*;

    #[test]
    fn reads_replaces_rotates_and_removes_pdf_pages() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("input.pdf");
        let replaced = temp.path().join("replaced.pdf");
        let rotated = temp.path().join("rotated.pdf");
        let reduced = temp.path().join("reduced.pdf");
        create_test_pdf(&input, &["Hello Finn", "Second page"]);

        let text = read_pdf(&input).unwrap();
        assert!(text.contains("pages: 2"));
        assert!(text.contains("Hello Finn"));

        replace_pdf_text(&input, &replaced, 1, "Finn", "World", false).unwrap();
        assert!(read_pdf(&replaced).unwrap().contains("Hello World"));

        transform_pdf_pages(&replaced, &rotated, "rotate", &[1], 90, false).unwrap();
        let rotated_doc = Document::load(&rotated).unwrap();
        let first_id = rotated_doc.get_pages()[&1];
        assert_eq!(
            rotated_doc
                .get_object(first_id)
                .unwrap()
                .as_dict()
                .unwrap()
                .get(b"Rotate")
                .unwrap()
                .as_i64()
                .unwrap(),
            90
        );

        transform_pdf_pages(&rotated, &reduced, "remove", &[2], 0, false).unwrap();
        assert_eq!(Document::load(&reduced).unwrap().get_pages().len(), 1);
    }

    fn create_test_pdf(path: &Path, texts: &[&str]) {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! {"F1" => font_id},
        });
        let mut page_ids = Vec::new();
        for text in texts {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 18.into()]),
                    Operation::new("Td", vec![72.into(), 720.into()]),
                    Operation::new("Tj", vec![Object::string_literal(*text)]),
                    Operation::new("ET", vec![]),
                ],
            };
            let content_id =
                document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
            let page_id = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            });
            page_ids.push(page_id);
        }
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
                "Count" => page_ids.len() as i64,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        document.compress();
        document.save(path).unwrap();
    }
}
