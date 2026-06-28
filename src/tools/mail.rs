use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

const MAIL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MAIL_OUTPUT: usize = 128 * 1024;

pub async fn search(query: &str, limit: usize) -> Result<String> {
    let script = r#"
on run argv
    set queryText to item 1 of argv
    set resultLimit to (item 2 of argv) as integer
    set outputLines to {}
    ignoring case
        tell application "Mail"
            repeat with messageItem in messages of inbox
                set messageSubject to subject of messageItem
                set messageSender to sender of messageItem
                if messageSubject contains queryText or messageSender contains queryText then
                    set end of outputLines to ((id of messageItem) as text) & tab & messageSender & tab & messageSubject & tab & ((date received of messageItem) as text)
                    if (count of outputLines) is greater than or equal to resultLimit then exit repeat
                end if
            end repeat
        end tell
    end ignoring
    return joinLines(outputLines)
end run

on joinLines(itemsList)
    set oldDelimiters to AppleScript's text item delimiters
    set AppleScript's text item delimiters to linefeed
    set joinedText to itemsList as text
    set AppleScript's text item delimiters to oldDelimiters
    return joinedText
end joinLines
"#;
    let output = run_osascript(script, &[query, &limit.clamp(1, 100).to_string()]).await?;
    Ok(if output.trim().is_empty() {
        "no matching inbox messages".to_owned()
    } else {
        format!("id\tsender\tsubject\tdate\n{output}")
    })
}

pub async fn read(message_id: u64) -> Result<String> {
    let script = r#"
on run argv
    set targetId to (item 1 of argv) as integer
    tell application "Mail"
        repeat with messageItem in messages of inbox
            if (id of messageItem) is targetId then
                return "from: " & (sender of messageItem) & linefeed & "subject: " & (subject of messageItem) & linefeed & "date: " & ((date received of messageItem) as text) & linefeed & linefeed & (content of messageItem)
            end if
        end repeat
    end tell
    return "message not found"
end run
"#;
    run_osascript(script, &[&message_id.to_string()]).await
}

pub async fn send(to: &str, subject: &str, body: &str, attachments: &[PathBuf]) -> Result<String> {
    for attachment in attachments {
        validate_attachment(attachment).await?;
    }
    let preferred_sender =
        env::var("FINN_MAIL_SENDER").unwrap_or_else(|_| "makisf4@gmail.com".to_owned());
    let script = r#"
on run argv
    set recipientAddress to item 1 of argv
    set messageSubject to item 2 of argv
    set messageBody to item 3 of argv
    set senderAddress to item 4 of argv
    tell application "Mail"
        if senderAddress is "" then
            repeat with accountItem in accounts
                if enabled of accountItem then
                    set configuredAddresses to email addresses of accountItem
                    if (count of configuredAddresses) is greater than 0 then
                        set senderAddress to item 1 of configuredAddresses
                        exit repeat
                    end if
                end if
            end repeat
        end if
        if senderAddress is "" then error "No enabled Apple Mail sender account is configured."

        set outgoingMessage to make new outgoing message with properties {sender:senderAddress, subject:messageSubject, content:messageBody & return & return, visible:false}
        tell outgoingMessage
            make new to recipient at end of to recipients with properties {address:recipientAddress}
            repeat with argumentIndex from 5 to count of argv
                set attachmentPath to item argumentIndex of argv
                set attachmentFile to POSIX file attachmentPath
                tell content
                    make new attachment with properties {file name:attachmentFile} at after last paragraph
                end tell
            end repeat
            set sendSucceeded to send
        end tell
        if sendSucceeded is false then error "Apple Mail rejected the send request."
    end tell
    return "accepted"
end run
"#;
    let attachment_args = attachments
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut args = vec![to, subject, body, preferred_sender.as_str()];
    args.extend(attachment_args.iter().map(String::as_str));
    let result = run_osascript(script, &args).await?;
    if result.trim() != "accepted" {
        bail!("Apple Mail returned an unexpected send result: {result}");
    }
    Ok(format!(
        "status: accepted_by_apple_mail\nto: {to}\nsubject: {subject}\nsender: {}\nattachments: {}",
        preferred_sender,
        attachments.len()
    ))
}

async fn validate_attachment(path: &Path) -> Result<()> {
    let metadata = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("attachment does not exist: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("attachment is not a file: {}", path.display());
    }
    Ok(())
}

async fn run_osascript(script: &str, args: &[&str]) -> Result<String> {
    if !cfg!(target_os = "macos") {
        bail!("Apple Mail tools require macOS.");
    }

    let mut command = Command::new("/usr/bin/osascript");
    command
        .arg("-")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().context("cannot start osascript")?;
    let mut stdin = child.stdin.take().context("cannot open osascript stdin")?;
    stdin.write_all(script.as_bytes()).await?;
    drop(stdin);

    let output = timeout(MAIL_TIMEOUT, child.wait_with_output())
        .await
        .context("Apple Mail operation timed out")??;
    let stdout = clipped(&output.stdout);
    let stderr = clipped(&output.stderr);
    if !output.status.success() {
        bail!(
            "Apple Mail operation failed: {}",
            if stderr.is_empty() {
                "unknown osascript error"
            } else {
                &stderr
            }
        );
    }
    Ok(stdout)
}

fn clipped(bytes: &[u8]) -> String {
    let truncated = bytes.len() > MAX_MAIL_OUTPUT;
    let slice = &bytes[..bytes.len().min(MAX_MAIL_OUTPUT)];
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
    async fn validates_attachment_files_before_opening_mail() {
        let temp = tempfile::tempdir().unwrap();
        let report = temp.path().join("report.xlsx");
        tokio::fs::write(&report, b"workbook").await.unwrap();

        assert!(validate_attachment(&report).await.is_ok());
        assert!(validate_attachment(temp.path()).await.is_err());
        assert!(
            validate_attachment(&temp.path().join("missing.xlsx"))
                .await
                .is_err()
        );
    }
}
