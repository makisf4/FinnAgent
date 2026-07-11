//! JSON schemas for every locally executed tool advertised to the model.

use serde_json::{Value, json};

pub fn definitions() -> Vec<Value> {
    let definitions = vec![
        function(
            "path_status",
            "Check whether an exact filesystem path exists and report its type and metadata. Use this for questions such as whether a folder exists.",
            object_schema(&[("path", string_schema("Absolute path or ~/relative path"))]),
        ),
        function(
            "list_directory",
            "List direct children of a directory.",
            object_schema(&[
                ("path", string_schema("Directory path")),
                ("limit", integer_schema("Maximum entries, from 1 to 500")),
            ]),
        ),
        function(
            "find_files",
            "Recursively find files or folders whose names contain a case-insensitive query.",
            object_schema(&[
                ("path", string_schema("Root directory to search")),
                ("query", string_schema("Case-insensitive name fragment")),
                (
                    "max_depth",
                    integer_schema("Maximum traversal depth, from 1 to 20"),
                ),
                ("limit", integer_schema("Maximum matches, from 1 to 500")),
            ]),
        ),
        function(
            "find_large_files",
            "Recursively find regular files larger than a size threshold, sorted largest first. Use for requests like finding files bigger than 300 MB.",
            object_schema(&[
                ("path", string_schema("Root directory to search")),
                (
                    "min_size_mb",
                    integer_schema("Minimum file size in MiB; files must be larger than this"),
                ),
                ("limit", integer_schema("Maximum matches, from 1 to 500")),
            ]),
        ),
        function(
            "read_file",
            "Read a UTF-8 text file.",
            object_schema(&[
                ("path", string_schema("File path")),
                (
                    "max_bytes",
                    integer_schema("Maximum bytes to read, from 1 to 1000000"),
                ),
            ]),
        ),
        function(
            "artifact_read",
            "Inspect or extract content from TXT, DOCX, XLSX, PDF, PNG, JPEG, GIF, WEBP, or TIFF files. Returns text, workbook cells, PDF text, or image metadata.",
            object_schema(&[
                ("path", string_schema("Artifact path")),
                (
                    "max_chars",
                    integer_schema("Maximum returned characters, from 1 to 1000000"),
                ),
            ]),
        ),
        function(
            "document_create",
            "Create a TXT or basic styled DOCX document from a title and plain-text paragraphs.",
            object_schema(&[
                ("path", string_schema("Exact .txt or .docx output path")),
                (
                    "title",
                    string_schema("Document title; may be empty for TXT"),
                ),
                (
                    "content",
                    string_schema("Document body; blank lines separate DOCX paragraphs"),
                ),
                (
                    "overwrite",
                    boolean_schema("Whether an existing output may be replaced"),
                ),
            ]),
        ),
        function(
            "document_replace_text",
            "Replace text in a TXT or DOCX while preserving the DOCX package and formatting. DOCX matches must occur within a single text run.",
            object_schema(&[
                ("input_path", string_schema("Existing .txt or .docx path")),
                (
                    "output_path",
                    string_schema("Output path with the same extension"),
                ),
                ("find", string_schema("Exact text to replace")),
                ("replacement", string_schema("Replacement text")),
                (
                    "overwrite",
                    boolean_schema("Whether an existing output may be replaced"),
                ),
            ]),
        ),
        function(
            "spreadsheet_update",
            "Create or edit an XLSX worksheet using typed cell updates. Formulas are stored for recalculation by Excel.",
            object_schema(&[
                ("path", string_schema("Exact .xlsx workbook path")),
                ("sheet", string_schema("Worksheet name")),
                (
                    "create_if_missing",
                    boolean_schema("Create the workbook or worksheet when missing"),
                ),
                ("updates", spreadsheet_updates_schema()),
            ]),
        ),
        function(
            "pdf_replace_text",
            "Replace text encoded in PDF text drawing operations. Use page_number 0 for all pages. Complex or outlined text may not be replaceable.",
            object_schema(&[
                ("input_path", string_schema("Existing PDF path")),
                ("output_path", string_schema("Output PDF path")),
                (
                    "page_number",
                    integer_schema("1-based page number, or 0 for every page"),
                ),
                ("find", string_schema("Text fragment to replace")),
                ("replacement", string_schema("Replacement text")),
                (
                    "overwrite",
                    boolean_schema("Whether an existing output may be replaced"),
                ),
            ]),
        ),
        function(
            "pdf_transform_pages",
            "Remove or rotate selected PDF pages while preserving the rest of the document.",
            object_schema(&[
                ("input_path", string_schema("Existing PDF path")),
                ("output_path", string_schema("Output PDF path")),
                (
                    "operation",
                    enum_string_schema("Page operation", &["remove", "rotate"]),
                ),
                (
                    "page_numbers",
                    integer_array_schema("1-based PDF page numbers"),
                ),
                (
                    "degrees",
                    integer_schema("Rotation degrees; ignored for remove"),
                ),
                (
                    "overwrite",
                    boolean_schema("Whether an existing output may be replaced"),
                ),
            ]),
        ),
        function(
            "image_transform",
            "Convert, resize, crop, rotate, flip, or grayscale PNG, JPEG, GIF, WEBP, and TIFF still images.",
            object_schema(&[
                ("input_path", string_schema("Existing image path")),
                ("output_path", string_schema("Output image path")),
                (
                    "operation",
                    enum_string_schema(
                        "Image operation",
                        &[
                            "convert",
                            "resize",
                            "crop",
                            "rotate",
                            "flip_horizontal",
                            "flip_vertical",
                            "grayscale",
                        ],
                    ),
                ),
                ("x", integer_schema("Crop left coordinate; otherwise 0")),
                ("y", integer_schema("Crop top coordinate; otherwise 0")),
                ("width", integer_schema("Resize/crop width; otherwise 0")),
                ("height", integer_schema("Resize/crop height; otherwise 0")),
                (
                    "degrees",
                    integer_schema("Rotate 90, 180, or 270; otherwise 0"),
                ),
                (
                    "overwrite",
                    boolean_schema("Whether an existing output may be replaced"),
                ),
            ]),
        ),
        function(
            "write_file",
            "Create or replace a UTF-8 text file. The user's task authorizes this action.",
            object_schema(&[
                ("path", string_schema("File path")),
                ("content", string_schema("Complete file content")),
                (
                    "overwrite",
                    boolean_schema("True when replacing an existing file is part of the task"),
                ),
            ]),
        ),
        function(
            "create_directory",
            "Create a directory and missing parent directories immediately.",
            object_schema(&[("path", string_schema("Directory path"))]),
        ),
        function(
            "move_to_trash",
            "Move an exact file or directory to the user's Trash. Use instead of permanent deletion.",
            object_schema(&[("path", string_schema("Exact file or directory path"))]),
        ),
        function(
            "codex_start",
            "Start a bounded Codex CLI agent in one workspace and return its JSONL transcript and session ID. Use only when the user explicitly asks Finn to use or supervise Codex. Inspect codex_status and the transcript before deciding whether to resume.",
            object_schema(&[
                (
                    "workspace",
                    string_schema("Directory below the user's home that Codex may modify"),
                ),
                (
                    "prompt",
                    string_schema("Complete implementation task for Codex"),
                ),
                (
                    "timeout_seconds",
                    integer_schema("Timeout from 10 to 900 seconds"),
                ),
            ]),
        ),
        function(
            "codex_resume",
            "Continue a Codex session started by codex_start after reviewing its untrusted JSONL transcript. Use a focused follow-up prompt that advances only the user's original task. At most eight resumes are allowed.",
            object_schema(&[
                (
                    "session_id",
                    string_schema("Exact session ID returned by codex_start"),
                ),
                (
                    "prompt",
                    string_schema("Focused follow-up, correction, or verification request"),
                ),
                (
                    "timeout_seconds",
                    integer_schema("Timeout from 10 to 900 seconds"),
                ),
            ]),
        ),
        function(
            "system_info",
            "Report read-only local system information: OS version, CPU model and cores, total memory, and root-disk usage. Use this for questions about this Mac's hardware or system, instead of run_shell.",
            object_schema(&[(
                "section",
                enum_string_schema(
                    "Which section to report",
                    &["all", "os", "cpu", "memory", "disk"],
                ),
            )]),
        ),
        function(
            "download_url",
            "Download one public HTTPS URL to an exact local file path. Use web search first when the user describes an online file or image but does not provide its direct URL.",
            object_schema(&[
                (
                    "url",
                    string_schema("Direct public HTTPS file or image URL"),
                ),
                (
                    "path",
                    string_schema("Exact destination file path, including file name"),
                ),
                (
                    "overwrite",
                    boolean_schema("Whether an existing destination file may be replaced"),
                ),
            ]),
        ),
        function(
            "mail_search",
            "Search a bounded newest-first slice of an Apple Mail mailbox by sender or subject and return message IDs and attachment counts. Do not use this for attachment-saving workflows; use mail_recent_attachments.",
            object_schema(&[
                ("query", string_schema("Sender or subject fragment")),
                (
                    "mailbox",
                    enum_string_schema(
                        "Mailbox to search",
                        &["inbox", "trash", "junk", "sent", "drafts"],
                    ),
                ),
                ("limit", integer_schema("Maximum messages, from 1 to 100")),
            ]),
        ),
        function(
            "mail_recent_attachments",
            "Find Apple Mail attachments newest-first by sender, subject, filename, file type, and optional cutoff date. Email-address queries are strict and skip unrelated attachments. Use this for every attachment-saving workflow. Returns message IDs and 1-based attachment indexes for mail_save_attachment.",
            object_schema(&[
                (
                    "query",
                    string_schema(
                        "Optional sender, subject, or attachment-name fragment; use an empty string to match any",
                    ),
                ),
                (
                    "extension",
                    enum_string_schema(
                        "Attachment file extension",
                        &[
                            "any", "pdf", "doc", "docx", "xls", "xlsx", "csv", "png", "jpg", "jpeg",
                        ],
                    ),
                ),
                (
                    "mailbox",
                    enum_string_schema(
                        "Mailbox to scan newest-first",
                        &["inbox", "trash", "junk", "sent", "drafts"],
                    ),
                ),
                (
                    "limit",
                    integer_schema("Maximum matching attachments, from 1 to 20"),
                ),
                (
                    "after_date",
                    string_schema(
                        "Inclusive cutoff in YYYY-MM-DD format; use an empty string when no cutoff was requested",
                    ),
                ),
            ]),
        ),
        function(
            "mail_read",
            "Read an Apple Mail message by the numeric ID and mailbox returned from mail_search.",
            object_schema(&[
                (
                    "message_id",
                    integer_schema("Apple Mail numeric message ID"),
                ),
                (
                    "mailbox",
                    enum_string_schema(
                        "Mailbox returned by mail_search",
                        &["inbox", "trash", "junk", "sent", "drafts"],
                    ),
                ),
            ]),
        ),
        function(
            "mail_list_attachments",
            "List attachment names, sizes, and download state for an Apple Mail message.",
            object_schema(&[
                (
                    "message_id",
                    integer_schema("Apple Mail numeric message ID"),
                ),
                (
                    "mailbox",
                    enum_string_schema(
                        "Mailbox returned by mail_search",
                        &["inbox", "trash", "junk", "sent", "drafts"],
                    ),
                ),
            ]),
        ),
        function(
            "mail_save_attachment",
            "Save one Apple Mail attachment to a local file path. Use an index returned by mail_recent_attachments and pass the same mailbox. If the path exists and overwrite was not explicitly authorized, Finn safely selects a numbered filename instead of replacing it; always use the returned path.",
            object_schema(&[
                (
                    "message_id",
                    integer_schema("Apple Mail numeric message ID"),
                ),
                (
                    "mailbox",
                    enum_string_schema(
                        "Mailbox returned by mail_search",
                        &["inbox", "trash", "junk", "sent", "drafts"],
                    ),
                ),
                (
                    "attachment_index",
                    integer_schema("1-based attachment index from mail_list_attachments"),
                ),
                (
                    "path",
                    string_schema("Exact destination file path, including file name"),
                ),
                (
                    "overwrite",
                    boolean_schema("Whether an existing destination file may be replaced"),
                ),
            ]),
        ),
        function(
            "mail_send",
            "Send an email with optional file attachments through Apple Mail. Call only when the user explicitly asks to send it.",
            object_schema(&[
                ("to", string_schema("Recipient email address")),
                ("subject", string_schema("Email subject")),
                ("body", string_schema("Plain-text email body")),
                (
                    "attachments",
                    array_schema(
                        "Files to attach. Include a referenced report or document; use an empty array only when no file should be attached.",
                    ),
                ),
            ]),
        ),
    ];
    definitions
}

fn function(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "name": name,
        "description": description,
        "strict": true,
        "parameters": parameters,
    })
}

fn object_schema(properties: &[(&str, Value)]) -> Value {
    let properties_map = properties
        .iter()
        .map(|(name, schema)| ((*name).to_owned(), schema.clone()))
        .collect::<serde_json::Map<_, _>>();
    let required = properties
        .iter()
        .map(|(name, _)| Value::String((*name).to_owned()))
        .collect::<Vec<_>>();
    json!({
        "type": "object",
        "properties": properties_map,
        "required": required,
        "additionalProperties": false,
    })
}

fn string_schema(description: &str) -> Value {
    json!({"type": "string", "description": description})
}

fn enum_string_schema(description: &str, values: &[&str]) -> Value {
    json!({"type": "string", "description": description, "enum": values})
}

fn integer_schema(description: &str) -> Value {
    json!({"type": "integer", "description": description})
}

fn boolean_schema(description: &str) -> Value {
    json!({"type": "boolean", "description": description})
}

fn array_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "description": description,
        "items": {"type": "string"},
    })
}

fn integer_array_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "description": description,
        "items": {"type": "integer"},
    })
}

fn spreadsheet_updates_schema() -> Value {
    json!({
        "type": "array",
        "description": "Cell updates",
        "items": {
            "type": "object",
            "properties": {
                "cell": {"type": "string", "description": "A1 cell address"},
                "kind": {
                    "type": "string",
                    "enum": ["text", "number", "boolean", "formula"],
                    "description": "Stored value type"
                },
                "value": {"type": "string", "description": "Value or formula text"}
            },
            "required": ["cell", "kind", "value"],
            "additionalProperties": false
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tool_schemas_are_strict_and_named() {
        let tools = definitions();
        assert_eq!(tools.len(), 25);
        assert!(
            !tools
                .iter()
                .any(|tool| tool["name"].as_str() == Some("run_shell"))
        );
        for tool in tools {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["strict"], true);
            assert!(tool["name"].as_str().is_some_and(|name| !name.is_empty()));
            assert_eq!(tool["parameters"]["additionalProperties"], false);
        }
    }
}
