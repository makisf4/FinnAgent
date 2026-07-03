use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::timeout;

use super::filesystem;

const CODEX_CANDIDATES: &[&str] = &[
    "/opt/homebrew/bin/codex",
    "/usr/local/bin/codex",
    "/usr/bin/codex",
];
const MAX_PROMPT_BYTES: usize = 32 * 1024;
const MAX_STDOUT_BYTES: usize = 64 * 1024;
const MAX_STDERR_BYTES: usize = 16 * 1024;
const MAX_RESUMES: u8 = 8;

#[derive(Clone, Default)]
pub struct SessionStore {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

#[derive(Clone)]
struct Session {
    workspace: PathBuf,
    resumes: u8,
}

pub async fn start(
    sessions: &SessionStore,
    home: &Path,
    workspace: &Path,
    prompt: &str,
    timeout_seconds: u64,
) -> Result<String> {
    validate_prompt(prompt)?;
    let workspace = prepare_workspace(home, workspace).await?;
    let codex = codex_binary()?;
    let args = [
        "exec",
        "--json",
        "--skip-git-repo-check",
        "--sandbox",
        "workspace-write",
        "--cd",
        workspace
            .to_str()
            .context("Codex workspace path is not valid UTF-8")?,
        prompt,
    ];
    let execution = execute(codex, &args, &workspace, timeout_seconds).await?;
    let session_id = transcript_session_id(&execution.stdout);
    if let Some(id) = &session_id {
        sessions.sessions.lock().await.insert(
            id.clone(),
            Session {
                workspace: workspace.clone(),
                resumes: 0,
            },
        );
    }
    Ok(render_result(&workspace, session_id.as_deref(), &execution))
}

pub async fn resume(
    sessions: &SessionStore,
    session_id: &str,
    prompt: &str,
    timeout_seconds: u64,
) -> Result<String> {
    validate_prompt(prompt)?;
    let workspace = {
        let mut guard = sessions.sessions.lock().await;
        let session = guard
            .get_mut(session_id)
            .context("unknown Codex session; start it with codex_start in this Finn process")?;
        if session.resumes >= MAX_RESUMES {
            bail!("Codex session reached the limit of {MAX_RESUMES} resume calls");
        }
        session.resumes += 1;
        session.workspace.clone()
    };
    let codex = codex_binary()?;
    let args = [
        "exec",
        "resume",
        "--json",
        "--skip-git-repo-check",
        session_id,
        prompt,
    ];
    let execution = execute(codex, &args, &workspace, timeout_seconds).await?;
    Ok(render_result(&workspace, Some(session_id), &execution))
}

async fn prepare_workspace(home: &Path, workspace: &Path) -> Result<PathBuf> {
    filesystem::ensure_not_sensitive(workspace, home)?;
    let canonical_home = tokio::fs::canonicalize(home)
        .await
        .with_context(|| format!("cannot resolve home directory {}", home.display()))?;
    ensure_workspace_location(workspace, home, &canonical_home)?;
    tokio::fs::create_dir_all(workspace)
        .await
        .with_context(|| format!("cannot create Codex workspace {}", workspace.display()))?;
    let canonical_workspace = tokio::fs::canonicalize(workspace)
        .await
        .with_context(|| format!("cannot resolve Codex workspace {}", workspace.display()))?;
    if canonical_workspace == canonical_home || !canonical_workspace.starts_with(&canonical_home) {
        bail!(
            "Codex workspace must be a directory below the user's home: {}",
            workspace.display()
        );
    }
    filesystem::ensure_not_sensitive(&canonical_workspace, &canonical_home)?;
    Ok(canonical_workspace)
}

fn ensure_workspace_location(workspace: &Path, home: &Path, canonical_home: &Path) -> Result<()> {
    let relative = workspace.strip_prefix(home).with_context(|| {
        format!(
            "Codex workspace must be below the user's home: {}",
            workspace.display()
        )
    })?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| name.starts_with('.'))
        })
    {
        bail!(
            "Codex workspace must be a non-hidden directory below the user's home: {}",
            workspace.display()
        );
    }

    let mut existing = workspace;
    while !existing.exists() {
        existing = existing.parent().with_context(|| {
            format!(
                "cannot find an existing parent for Codex workspace {}",
                workspace.display()
            )
        })?;
    }
    let canonical_existing = std::fs::canonicalize(existing)
        .with_context(|| format!("cannot resolve workspace parent {}", existing.display()))?;
    if !canonical_existing.starts_with(canonical_home) {
        bail!(
            "Codex workspace resolves outside the user's home: {}",
            workspace.display()
        );
    }
    Ok(())
}

fn validate_prompt(prompt: &str) -> Result<()> {
    if prompt.trim().is_empty() {
        bail!("Codex prompt cannot be empty");
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        bail!("Codex prompt exceeds the {MAX_PROMPT_BYTES}-byte limit");
    }
    Ok(())
}

fn codex_binary() -> Result<&'static Path> {
    CODEX_CANDIDATES
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .context("Codex CLI was not found in a supported installation path")
}

struct Execution {
    exit_code: i32,
    stdout: Capture,
    stderr: Capture,
}

struct Capture {
    text: String,
    truncated: bool,
}

async fn execute(
    codex: &Path,
    args: &[&str],
    workspace: &Path,
    timeout_seconds: u64,
) -> Result<Execution> {
    let mut child = Command::new(codex);
    child
        .args(args)
        .current_dir(workspace)
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = child
        .spawn()
        .with_context(|| format!("cannot start Codex CLI at {}", codex.display()))?;
    let stdout = child.stdout.take().context("cannot capture Codex stdout")?;
    let stderr = child.stderr.take().context("cannot capture Codex stderr")?;
    let stdout_task = tokio::spawn(read_limited(stdout, MAX_STDOUT_BYTES));
    let stderr_task = tokio::spawn(read_limited(stderr, MAX_STDERR_BYTES));
    let duration = Duration::from_secs(timeout_seconds.clamp(10, 900));
    let status = match timeout(duration, child.wait()).await {
        Ok(status) => status.context("cannot wait for Codex CLI")?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            bail!("Codex CLI timed out after {} seconds", duration.as_secs());
        }
    };
    let stdout = stdout_task
        .await
        .context("Codex stdout reader stopped unexpectedly")??;
    let stderr = stderr_task
        .await
        .context("Codex stderr reader stopped unexpectedly")??;
    Ok(Execution {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

async fn read_limited<R>(mut reader: R, limit: usize) -> Result<Capture>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(captured.len());
        let retained = remaining.min(read);
        captured.extend_from_slice(&buffer[..retained]);
        truncated |= retained < read;
    }
    Ok(Capture {
        text: String::from_utf8_lossy(&captured).trim().to_owned(),
        truncated,
    })
}

fn transcript_session_id(capture: &Capture) -> Option<String> {
    capture
        .text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| find_string(&value, &["thread_id", "session_id"]))
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    return Some(value.to_owned());
                }
            }
            map.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(values) => values.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn render_result(workspace: &Path, session_id: Option<&str>, execution: &Execution) -> String {
    let mut result = format!(
        "codex_status: {}\nexit_code: {}\nsession_id: {}\nworkspace: {}",
        if execution.exit_code == 0 {
            "complete"
        } else {
            "failed"
        },
        execution.exit_code,
        session_id.unwrap_or("unavailable"),
        workspace.display()
    );
    if !execution.stdout.text.is_empty() {
        result.push_str("\njsonl_transcript:\n");
        result.push_str(&execution.stdout.text);
        if execution.stdout.truncated {
            result.push_str("\n[Codex stdout truncated]");
        }
    }
    if !execution.stderr.text.is_empty() {
        result.push_str("\nstderr:\n");
        result.push_str(&execution.stderr.text);
        if execution.stderr.truncated {
            result.push_str("\n[Codex stderr truncated]");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_thread_id_from_jsonl() {
        let capture = Capture {
            text: concat!(
                "{\"type\":\"thread.started\",\"thread_id\":\"abc-123\"}\n",
                "{\"type\":\"turn.completed\"}"
            )
            .to_owned(),
            truncated: false,
        };
        assert_eq!(transcript_session_id(&capture).as_deref(), Some("abc-123"));
    }

    #[tokio::test]
    async fn workspace_must_remain_below_home() {
        let home = tempfile::tempdir().unwrap();
        let nested = home.path().join("Desktop/app");
        let prepared = prepare_workspace(home.path(), &nested).await.unwrap();
        assert!(prepared.starts_with(home.path().canonicalize().unwrap()));
        assert!(prepare_workspace(home.path(), home.path()).await.is_err());
        assert!(
            prepare_workspace(home.path(), &home.path().join(".config/app"))
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_symlink_cannot_escape_before_creation() {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = home.path().join("Desktop");
        symlink(outside.path(), &link).unwrap();
        let escaped = link.join("should-not-exist");
        assert!(prepare_workspace(home.path(), &escaped).await.is_err());
        assert!(!outside.path().join("should-not-exist").exists());
    }

    #[tokio::test]
    async fn limited_reader_discards_excess_bytes() {
        let input = std::io::Cursor::new(b"123456789".to_vec());
        let capture = read_limited(input, 4).await.unwrap();
        assert_eq!(capture.text, "1234");
        assert!(capture.truncated);
    }
}
