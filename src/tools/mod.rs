mod filesystem;
mod mail;
mod shell;

use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TaskAuthorization {
    allow_mail_send: bool,
    allow_trash: bool,
}

impl TaskAuthorization {
    pub fn from_task(task: &str) -> Self {
        let task = task.to_lowercase();
        let mail_action = contains_any(
            &task,
            &["send", "email ", "mail ", "στείλ", "στειλ", "αποστολ"],
        );
        let mail_object = contains_any(&task, &["email", "mail", "message", "μήνυμα", "μηνυμα"]);
        let allow_trash = contains_any(
            &task,
            &[
                "delete",
                "remove",
                "trash",
                "move to trash",
                "διαγρα",
                "σβήσ",
                "σβησ",
                "κάδο",
                "καδο",
            ],
        );
        Self {
            allow_mail_send: mail_action && mail_object,
            allow_trash,
        }
    }

    fn require_mail_send(self) -> Result<()> {
        if !self.allow_mail_send {
            bail!("mail_send denied: the original user task did not explicitly authorize email");
        }
        Ok(())
    }

    fn require_trash(self) -> Result<()> {
        if !self.allow_trash {
            bail!(
                "move_to_trash denied: the original user task did not explicitly authorize deletion"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    home: PathBuf,
    data_dir: PathBuf,
}

impl ToolContext {
    pub fn new(home: PathBuf, data_dir: PathBuf) -> Self {
        Self { home, data_dir }
    }

    pub async fn execute(
        &self,
        name: &str,
        arguments: &str,
        authorization: TaskAuthorization,
    ) -> String {
        match self.execute_inner(name, arguments, authorization).await {
            Ok(value) => value,
            Err(error) => format!("ERROR: {error:#}"),
        }
    }

    async fn execute_inner(
        &self,
        name: &str,
        arguments: &str,
        authorization: TaskAuthorization,
    ) -> Result<String> {
        let args: Value = serde_json::from_str(arguments)
            .with_context(|| format!("invalid arguments for tool {name}"))?;

        match name {
            "path_status" => filesystem::path_status(&self.path_arg(&args)?).await,
            "list_directory" => {
                filesystem::ensure_not_sensitive(&self.path_arg(&args)?, &self.home)?;
                filesystem::list_directory(
                    &self.path_arg(&args)?,
                    required_u64(&args, "limit")? as usize,
                )
                .await
            }
            "find_files" => {
                filesystem::ensure_not_sensitive(&self.path_arg(&args)?, &self.home)?;
                filesystem::find_files(
                    &self.path_arg(&args)?,
                    required_str(&args, "query")?,
                    required_u64(&args, "max_depth")? as usize,
                    required_u64(&args, "limit")? as usize,
                )
                .await
            }
            "read_file" => {
                filesystem::ensure_not_sensitive(&self.path_arg(&args)?, &self.home)?;
                filesystem::read_file(
                    &self.path_arg(&args)?,
                    required_u64(&args, "max_bytes")? as usize,
                )
                .await
            }
            "write_file" => {
                filesystem::ensure_not_sensitive(&self.path_arg(&args)?, &self.home)?;
                filesystem::write_file(
                    &self.path_arg(&args)?,
                    required_str(&args, "content")?,
                    required_bool(&args, "overwrite")?,
                )
                .await
            }
            "create_directory" => {
                filesystem::ensure_not_sensitive(&self.path_arg(&args)?, &self.home)?;
                filesystem::create_directory(&self.path_arg(&args)?).await
            }
            "move_to_trash" => {
                authorization.require_trash()?;
                filesystem::ensure_not_sensitive(&self.path_arg(&args)?, &self.home)?;
                filesystem::move_to_trash(&self.path_arg(&args)?, &self.home.join(".Trash")).await
            }
            "run_shell" => {
                shell::run(
                    required_str(&args, "command")?,
                    &self.home,
                    required_u64(&args, "timeout_seconds")?,
                )
                .await
            }
            "mail_search" => {
                mail::search(
                    required_str(&args, "query")?,
                    required_u64(&args, "limit")? as usize,
                )
                .await
            }
            "mail_read" => mail::read(required_u64(&args, "message_id")?).await,
            "mail_send" => {
                authorization.require_mail_send()?;
                let attachments = required_string_array(&args, "attachments")?
                    .iter()
                    .map(|path| filesystem::resolve_path(path, &self.home))
                    .collect::<Vec<_>>();
                for attachment in &attachments {
                    filesystem::ensure_not_sensitive(attachment, &self.home)?;
                }
                mail::send(
                    required_str(&args, "to")?,
                    required_str(&args, "subject")?,
                    required_str(&args, "body")?,
                    &attachments,
                )
                .await
            }
            _ => bail!("unknown tool: {name}"),
        }
    }

    fn path_arg(&self, args: &Value) -> Result<PathBuf> {
        let raw = required_str(args, "path")?;
        Ok(filesystem::resolve_path(raw, &self.home))
    }

    pub async fn append_task_log(&self, task: &str, result: &str) -> Result<()> {
        if env::var("FINN_TASK_LOG")
            .is_ok_and(|value| matches!(value.to_ascii_lowercase().as_str(), "0" | "false" | "off"))
        {
            return Ok(());
        }
        let log_path = self.data_dir.join("tasks.jsonl");
        let record = json!({
            "task": task,
            "result": result,
        });
        let mut line = serde_json::to_string(&record)?;
        line.push('\n');
        use tokio::io::AsyncWriteExt;
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).append(true).mode(0o600);
        let mut file = options.open(&log_path).await?;
        tokio::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600)).await?;
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

fn contains_any(value: &str, fragments: &[&str]) -> bool {
    fragments.iter().any(|fragment| value.contains(fragment))
}

fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string argument: {name}"))
}

fn required_u64(args: &Value, name: &str) -> Result<u64> {
    args.get(name)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing integer argument: {name}"))
}

fn required_bool(args: &Value, name: &str) -> Result<bool> {
    args.get(name)
        .and_then(Value::as_bool)
        .with_context(|| format!("missing boolean argument: {name}"))
}

fn required_string_array<'a>(args: &'a Value, name: &str) -> Result<Vec<&'a str>> {
    args.get(name)
        .and_then(Value::as_array)
        .with_context(|| format!("missing array argument: {name}"))?
        .iter()
        .map(|item| {
            item.as_str()
                .with_context(|| format!("{name} must contain only strings"))
        })
        .collect()
}

pub fn definitions() -> Vec<Value> {
    vec![
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
            "run_shell",
            "Run a Bash/Zsh command or script on the Mac. The task itself authorizes execution. Catastrophic commands are blocked.",
            object_schema(&[
                ("command", string_schema("Complete zsh command or script")),
                (
                    "timeout_seconds",
                    integer_schema("Timeout from 1 to 600 seconds"),
                ),
            ]),
        ),
        function(
            "mail_search",
            "Search Apple Mail inbox messages by sender or subject and return message IDs.",
            object_schema(&[
                ("query", string_schema("Sender or subject fragment")),
                ("limit", integer_schema("Maximum messages, from 1 to 100")),
            ]),
        ),
        function(
            "mail_read",
            "Read an Apple Mail inbox message by the numeric ID returned from mail_search.",
            object_schema(&[(
                "message_id",
                integer_schema("Apple Mail numeric message ID"),
            )]),
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
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tool_schemas_are_strict_and_named() {
        let tools = definitions();
        assert_eq!(tools.len(), 11);
        for tool in tools {
            assert_eq!(tool["type"], "function");
            assert_eq!(tool["strict"], true);
            assert!(tool["name"].as_str().is_some_and(|name| !name.is_empty()));
            assert_eq!(tool["parameters"]["additionalProperties"], false);
        }
    }

    #[test]
    fn derives_high_impact_authorization_from_original_task() {
        let mail = TaskAuthorization::from_task("Send the report by email to Alex");
        assert!(mail.require_mail_send().is_ok());
        assert!(mail.require_trash().is_err());

        let trash = TaskAuthorization::from_task("Move that folder to Trash");
        assert!(trash.require_trash().is_ok());
        assert!(trash.require_mail_send().is_err());

        let read_only = TaskAuthorization::from_task("Find emails from Alex");
        assert!(read_only.require_mail_send().is_err());
        assert!(read_only.require_trash().is_err());
    }

    #[tokio::test]
    async fn enforces_high_impact_authorization_before_side_effects() {
        let directory = tempfile::tempdir().unwrap();
        let context = ToolContext::new(
            directory.path().to_path_buf(),
            directory.path().join("data"),
        );
        tokio::fs::create_dir_all(directory.path().join("data"))
            .await
            .unwrap();
        let target = directory.path().join("note.txt");
        tokio::fs::write(&target, b"keep").await.unwrap();
        let arguments = json!({"path": target.to_string_lossy()}).to_string();

        let denied = context
            .execute("move_to_trash", &arguments, TaskAuthorization::default())
            .await;
        assert!(denied.contains("original user task did not explicitly authorize"));
        assert!(target.exists());

        let allowed = context
            .execute(
                "move_to_trash",
                &arguments,
                TaskAuthorization::from_task("Delete note.txt"),
            )
            .await;
        assert!(allowed.contains("status: complete"));
        assert!(!target.exists());
    }
}
