use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use docx_rs::{Docx, Paragraph, Run};
use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, Writer};
use zip::ZipWriter;
use zip::read::ZipArchive;
use zip::write::SimpleFileOptions;

use super::{clipped, commit_output, ensure_input_file, extension, prepare_output};

pub fn read_artifact(path: &Path, max_chars: usize) -> Result<String> {
    ensure_input_file(path)?;
    let ext = extension(path)?;
    let content = match ext.as_str() {
        "txt" | "md" | "csv" | "tsv" | "json" | "xml" | "html" | "css" | "js" | "rs" | "py" => {
            std::fs::read_to_string(path)
                .with_context(|| format!("cannot read UTF-8 text from {}", path.display()))?
        }
        "docx" => extract_docx_text(path)?,
        "xlsx" => super::spreadsheet::read_spreadsheet(path, max_chars)?,
        "pdf" => super::pdf::read_pdf(path)?,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "tif" | "tiff" => {
            super::images::inspect_image(path)?
        }
        _ => bail!("unsupported artifact type '.{ext}'"),
    };
    Ok(clipped(content, max_chars))
}

pub fn create_document(path: &Path, title: &str, content: &str, overwrite: bool) -> Result<String> {
    let ext = extension(path)?;
    let temporary = prepare_output(path, overwrite)?;
    let result = match ext.as_str() {
        "txt" => std::fs::write(&temporary, content)
            .with_context(|| format!("cannot write {}", temporary.display())),
        "docx" => create_docx(&temporary, title, content),
        _ => bail!("document_create supports only .txt and .docx outputs"),
    };
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    commit_output(&temporary, path)?;
    let size = std::fs::metadata(path)?.len();
    Ok(format!(
        "status: complete\npath: {}\ntype: {ext}\nbytes: {size}",
        path.display()
    ))
}

pub fn replace_document_text(
    input: &Path,
    output: &Path,
    find: &str,
    replacement: &str,
    overwrite: bool,
) -> Result<String> {
    ensure_input_file(input)?;
    if find.is_empty() {
        bail!("find text must not be empty");
    }
    let input_ext = extension(input)?;
    if extension(output)? != input_ext {
        bail!("replacement output must use the same extension as the input");
    }
    let temporary = prepare_output(output, overwrite || input == output)?;
    let count = match input_ext.as_str() {
        "txt" => {
            let source = std::fs::read_to_string(input)
                .with_context(|| format!("cannot read UTF-8 text from {}", input.display()))?;
            let count = source.matches(find).count();
            if count == 0 {
                bail!("text was not found; no output was written");
            }
            std::fs::write(&temporary, source.replace(find, replacement))?;
            count
        }
        "docx" => replace_docx_text(input, &temporary, find, replacement)?,
        _ => bail!("document_replace_text supports only .txt and .docx files"),
    };
    commit_output(&temporary, output)?;
    Ok(format!(
        "status: complete\npath: {}\nreplacements: {count}",
        output.display()
    ))
}

fn create_docx(path: &Path, title: &str, content: &str) -> Result<()> {
    let mut document = Docx::new();
    if !title.trim().is_empty() {
        let title_run = Run::new()
            .add_text(title.trim())
            .bold()
            .size(32)
            .color("1F4E78");
        document = document.add_paragraph(Paragraph::new().add_run(title_run));
    }
    for paragraph in content.split("\n\n") {
        let text = paragraph.lines().collect::<Vec<_>>().join(" ");
        document = document
            .add_paragraph(Paragraph::new().add_run(Run::new().add_text(text.trim()).size(22)));
    }
    let file = File::create(path)?;
    document
        .build()
        .pack(file)
        .context("cannot serialize DOCX package")
}

fn extract_docx_text(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file).context("invalid DOCX ZIP package")?;
    let mut document = archive
        .by_name("word/document.xml")
        .context("DOCX is missing word/document.xml")?;
    let mut xml = String::new();
    document.read_to_string(&mut xml)?;

    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);
    let mut output = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event()? {
            Event::Start(event) => {
                in_text = event.name().as_ref().ends_with(b":t");
            }
            Event::Text(event) if in_text => output.push_str(&event.unescape()?),
            Event::End(event) => {
                let name = event.name();
                if name.as_ref().ends_with(b":t") {
                    in_text = false;
                } else if name.as_ref().ends_with(b":tc") {
                    output.push('\t');
                } else if name.as_ref().ends_with(b":p") || name.as_ref().ends_with(b":tr") {
                    output.push('\n');
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(output.trim().to_owned())
}

fn replace_docx_text(input: &Path, output: &Path, find: &str, replacement: &str) -> Result<usize> {
    let source = File::open(input)?;
    let mut archive = ZipArchive::new(source).context("invalid DOCX ZIP package")?;
    let destination = File::create(output)?;
    let mut writer = ZipWriter::new(destination);
    let mut replacements = 0_usize;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        let options = SimpleFileOptions::default()
            .compression_method(entry.compression())
            .unix_permissions(entry.unix_mode().unwrap_or(0o644));
        if entry.is_dir() {
            writer.add_directory(name, options)?;
            continue;
        }
        writer.start_file(name.clone(), options)?;
        if name == "word/document.xml" {
            let mut xml = String::new();
            entry.read_to_string(&mut xml)?;
            let (updated, count) = replace_xml_text_nodes(&xml, find, replacement)?;
            replacements += count;
            writer.write_all(&updated)?;
        } else {
            std::io::copy(&mut entry, &mut writer)?;
        }
    }
    writer.finish()?;
    if replacements == 0 {
        let _ = std::fs::remove_file(output);
        bail!("text was not found in a single DOCX text run; no output was written");
    }
    Ok(replacements)
}

fn replace_xml_text_nodes(xml: &str, find: &str, replacement: &str) -> Result<(Vec<u8>, usize)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::with_capacity(xml.len()));
    let mut in_text = false;
    let mut count = 0_usize;
    loop {
        match reader.read_event()? {
            Event::Start(event) => {
                in_text = event.name().as_ref().ends_with(b":t");
                writer.write_event(Event::Start(event.into_owned()))?;
            }
            Event::Text(event) if in_text => {
                let decoded = event.unescape()?.into_owned();
                count += decoded.matches(find).count();
                writer.write_event(Event::Text(BytesText::new(
                    &decoded.replace(find, replacement),
                )))?;
            }
            Event::End(event) => {
                if event.name().as_ref().ends_with(b":t") {
                    in_text = false;
                }
                writer.write_event(Event::End(event.into_owned()))?;
            }
            Event::Eof => break,
            event => writer.write_event(event.into_owned())?,
        }
    }
    Ok((writer.into_inner(), count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_reads_and_replaces_txt_and_docx() {
        let temp = tempfile::tempdir().unwrap();
        let txt = temp.path().join("note.txt");
        create_document(&txt, "", "Hello Finn", false).unwrap();
        assert_eq!(read_artifact(&txt, 100).unwrap(), "Hello Finn");
        replace_document_text(&txt, &txt, "Finn", "World", false).unwrap();
        assert_eq!(read_artifact(&txt, 100).unwrap(), "Hello World");

        let docx = temp.path().join("report.docx");
        let edited = temp.path().join("report-edited.docx");
        create_document(
            &docx,
            "Quarterly Report",
            "Revenue grew.\n\nNext steps.",
            false,
        )
        .unwrap();
        let extracted = read_artifact(&docx, 1_000).unwrap();
        assert!(extracted.contains("Quarterly Report"));
        assert!(extracted.contains("Revenue grew."));

        replace_document_text(&docx, &edited, "Revenue", "Profit", false).unwrap();
        assert!(
            read_artifact(&edited, 1_000)
                .unwrap()
                .contains("Profit grew.")
        );
    }

    #[test]
    fn xml_replacement_escapes_inserted_text() {
        let xml = r#"<w:p><w:r><w:t>A &amp; B</w:t></w:r></w:p>"#;
        let (updated, count) = replace_xml_text_nodes(xml, "A & B", "C < D").unwrap();
        let updated = String::from_utf8(updated).unwrap();
        assert_eq!(count, 1);
        assert!(updated.contains("C &lt; D"));
    }
}
