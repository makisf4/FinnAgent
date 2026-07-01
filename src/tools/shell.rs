use std::env;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tokio::time::timeout;

use crate::safety;

const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const SAFE_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

pub fn enabled() -> bool {
    env::var("FINN_ENABLE_SHELL").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub async fn run(command: &str, cwd: &Path, timeout_seconds: u64) -> Result<String> {
    safety::validate_shell(command)?;
    let timeout_seconds = timeout_seconds.clamp(1, 600);

    let mut process = Command::new("/bin/zsh");
    process
        // `-f` prevents shell startup files from restoring secrets after env_clear.
        .arg("-f")
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", cwd)
        .env("PATH", SAFE_PATH)
        .env("SHELL", "/bin/zsh")
        .env("LANG", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = timeout(Duration::from_secs(timeout_seconds), process.output())
        .await
        .context("shell command timed out")??;

    let stdout = clipped(&output.stdout);
    let stderr = clipped(&output.stderr);
    let exit_code = output.status.code().unwrap_or(-1);
    let mut result = format!("exit_code: {exit_code}");
    if !stdout.is_empty() {
        result.push_str("\nstdout:\n");
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        result.push_str("\nstderr:\n");
        result.push_str(&stderr);
    }
    if !output.status.success() {
        bail!("shell command failed\n{result}");
    }
    Ok(result)
}

fn clipped(bytes: &[u8]) -> String {
    let truncated = bytes.len() > MAX_OUTPUT_BYTES;
    let slice = &bytes[..bytes.len().min(MAX_OUTPUT_BYTES)];
    let mut value = String::from_utf8_lossy(slice).trim().to_owned();
    if truncated {
        value.push_str("\n[output truncated]");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn executes_shell_scripts_without_confirmation() {
        let temp = tempfile::tempdir().unwrap();
        let result = run(
            "mkdir -p scripted && printf 'ok' > scripted/result.txt && cat scripted/result.txt",
            temp.path(),
            10,
        )
        .await
        .unwrap();

        assert!(result.contains("exit_code: 0"));
        assert!(result.contains("ok"));
        assert!(temp.path().join("scripted/result.txt").exists());
    }

    #[tokio::test]
    async fn blocks_catastrophic_shell_scripts() {
        let temp = tempfile::tempdir().unwrap();
        assert!(run("rm -rf /", temp.path(), 10).await.is_err());
    }

    #[tokio::test]
    async fn child_shell_does_not_inherit_provider_secrets() {
        let temp = tempfile::tempdir().unwrap();
        let result = run(
            "test -z \"$OPENAI_API_KEY\" && test -z \"$OPENROUTER_API_KEY\"",
            temp.path(),
            10,
        )
        .await
        .unwrap();
        assert!(result.contains("exit_code: 0"));
    }
}
