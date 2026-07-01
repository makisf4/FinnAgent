use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use umya_spreadsheet::{new_file_empty_worksheet, reader, writer};

use super::{clipped, commit_output, ensure_input_file, extension, prepare_output};

pub fn read_spreadsheet(path: &Path, max_chars: usize) -> Result<String> {
    let book = reader::xlsx::read(path)
        .with_context(|| format!("cannot parse workbook {}", path.display()))?;
    let mut output = format!(
        "type: xlsx\npath: {}\nsheets: {}\n",
        path.display(),
        book.sheet_count()
    );
    for sheet in book.sheet_collection() {
        let (columns, rows) = sheet.highest_column_and_row();
        output.push_str(&format!(
            "\n[sheet: {} rows: {rows} columns: {columns}]\n",
            sheet.name()
        ));
        for row in 1..=rows.min(200) {
            let values = (1..=columns.min(50))
                .map(|column| sheet.formatted_value((column, row)))
                .collect::<Vec<_>>();
            output.push_str(&values.join("\t"));
            output.push('\n');
            if output.chars().count() >= max_chars {
                return Ok(clipped(output, max_chars));
            }
        }
    }
    Ok(output)
}

pub fn update_spreadsheet(
    path: &Path,
    sheet_name: &str,
    create_if_missing: bool,
    updates: &[Value],
) -> Result<String> {
    if extension(path)? != "xlsx" {
        bail!("spreadsheet_update requires an .xlsx path");
    }
    if updates.is_empty() {
        bail!("spreadsheet updates must not be empty");
    }
    let mut book = if path.exists() {
        ensure_input_file(path)?;
        reader::xlsx::read(path)?
    } else if create_if_missing {
        new_file_empty_worksheet()
    } else {
        bail!("workbook does not exist and create_if_missing is false");
    };
    if book.sheet_by_name(sheet_name).is_err() {
        if create_if_missing {
            book.new_sheet(sheet_name)?;
        } else {
            bail!("worksheet does not exist: {sheet_name}");
        }
    }
    let sheet = book.sheet_by_name_mut(sheet_name)?;
    for update in updates {
        let object = update
            .as_object()
            .context("each spreadsheet update must be an object")?;
        let address = object
            .get("cell")
            .and_then(Value::as_str)
            .context("spreadsheet update is missing string field 'cell'")?;
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .context("spreadsheet update is missing string field 'kind'")?;
        let value = object
            .get("value")
            .and_then(Value::as_str)
            .context("spreadsheet update is missing string field 'value'")?;
        let cell = sheet.cell_mut(address);
        match kind {
            "text" => {
                cell.set_value(value);
            }
            "number" => {
                cell.set_value_number(
                    value
                        .parse::<f64>()
                        .with_context(|| format!("invalid number for {address}: {value}"))?,
                );
            }
            "boolean" => {
                cell.set_value_bool(
                    value
                        .parse::<bool>()
                        .with_context(|| format!("invalid boolean for {address}: {value}"))?,
                );
            }
            "formula" => {
                cell.set_formula(value.trim_start_matches('='));
            }
            _ => bail!(
                "unsupported spreadsheet value kind '{kind}'; expected text, number, boolean, or formula"
            ),
        }
    }
    let temporary = prepare_output(path, path.exists())?;
    if let Err(error) = writer::xlsx::write(&book, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).context("cannot save workbook");
    }
    commit_output(&temporary, path)?;
    Ok(format!(
        "status: complete\npath: {}\nsheet: {sheet_name}\nupdated_cells: {}",
        path.display(),
        updates.len()
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn creates_updates_and_reads_xlsx() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("budget.xlsx");
        let updates = vec![
            json!({"cell": "A1", "kind": "text", "value": "Revenue"}),
            json!({"cell": "B1", "kind": "number", "value": "1250.5"}),
            json!({"cell": "C1", "kind": "boolean", "value": "true"}),
            json!({"cell": "D1", "kind": "formula", "value": "=B1*2"}),
        ];
        update_spreadsheet(&path, "Summary", true, &updates).unwrap();

        let book = reader::xlsx::read(&path).unwrap();
        let sheet = book.sheet_by_name("Summary").unwrap();
        assert_eq!(sheet.value("A1"), "Revenue");
        assert_eq!(sheet.value("B1"), "1250.5");
        assert_eq!(sheet.value("C1"), "TRUE");
        assert_eq!(sheet.cell("D1").unwrap().formula(), "B1*2");

        let extracted = read_spreadsheet(&path, 2_000).unwrap();
        assert!(extracted.contains("[sheet: Summary"));
        assert!(extracted.contains("Revenue"));
    }
}
