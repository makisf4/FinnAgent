use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

const MAIL_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MAIL_OUTPUT: usize = 128 * 1024;

pub async fn search(query: &str, mailbox_scope: &str, limit: usize) -> Result<String> {
    validate_mailbox_scope(mailbox_scope)?;
    let script = r#"
on run argv
    set queryText to item 1 of argv
    set resultLimit to (item 2 of argv) as integer
    set mailboxScope to item 3 of argv
    set outputLines to {}
    ignoring case
        tell application "Mail"
            if mailboxScope is "inbox" then
                set sourceMailbox to inbox
            else if mailboxScope is "trash" then
                set sourceMailbox to trash mailbox
            else if mailboxScope is "junk" then
                set sourceMailbox to junk mailbox
            else if mailboxScope is "sent" then
                set sourceMailbox to sent mailbox
            else
                set sourceMailbox to drafts mailbox
            end if
            repeat with messageItem in messages of sourceMailbox
                set messageSubject to subject of messageItem
                set messageSender to sender of messageItem
                if messageSubject contains queryText or messageSender contains queryText then
                    set attachmentCount to count of mail attachments of messageItem
                    set end of outputLines to ((id of messageItem) as text) & tab & messageSender & tab & messageSubject & tab & ((date received of messageItem) as text) & tab & (attachmentCount as text)
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
    let output = run_osascript(
        script,
        &[query, &limit.clamp(1, 100).to_string(), mailbox_scope],
    )
    .await?;
    Ok(if output.trim().is_empty() {
        format!("no matching messages in {mailbox_scope}")
    } else {
        format!("mailbox: {mailbox_scope}\nid\tsender\tsubject\tdate\tattachments\n{output}")
    })
}

pub async fn read(message_id: u64, mailbox_scope: &str) -> Result<String> {
    validate_mailbox_scope(mailbox_scope)?;
    let script = r#"
on run argv
    set targetId to (item 1 of argv) as integer
    set mailboxScope to item 2 of argv
    tell application "Mail"
        if mailboxScope is "inbox" then
            set sourceMailbox to inbox
        else if mailboxScope is "trash" then
            set sourceMailbox to trash mailbox
        else if mailboxScope is "junk" then
            set sourceMailbox to junk mailbox
        else if mailboxScope is "sent" then
            set sourceMailbox to sent mailbox
        else
            set sourceMailbox to drafts mailbox
        end if
        repeat with messageItem in messages of sourceMailbox
            if (id of messageItem) is targetId then
                return "from: " & (sender of messageItem) & linefeed & "subject: " & (subject of messageItem) & linefeed & "date: " & ((date received of messageItem) as text) & linefeed & linefeed & (content of messageItem)
            end if
        end repeat
    end tell
    return "message not found"
end run
"#;
    run_osascript(script, &[&message_id.to_string(), mailbox_scope]).await
}

pub async fn list_attachments(message_id: u64, mailbox_scope: &str) -> Result<String> {
    validate_mailbox_scope(mailbox_scope)?;
    let script = r#"
on run argv
    set targetId to (item 1 of argv) as integer
    set mailboxScope to item 2 of argv
    tell application "Mail"
        if mailboxScope is "inbox" then
            set sourceMailbox to inbox
        else if mailboxScope is "trash" then
            set sourceMailbox to trash mailbox
        else if mailboxScope is "junk" then
            set sourceMailbox to junk mailbox
        else if mailboxScope is "sent" then
            set sourceMailbox to sent mailbox
        else
            set sourceMailbox to drafts mailbox
        end if
        repeat with messageItem in messages of sourceMailbox
            if (id of messageItem) is targetId then
                set attachmentItems to mail attachments of messageItem
                if (count of attachmentItems) is 0 then return "no attachments"
                set outputLines to {}
                repeat with attachmentIndex from 1 to count of attachmentItems
                    set attachmentItem to item attachmentIndex of attachmentItems
                    set end of outputLines to (attachmentIndex as text) & tab & (name of attachmentItem) & tab & ((file size of attachmentItem) as text) & tab & ((downloaded of attachmentItem) as text)
                end repeat
                return joinLines(outputLines)
            end if
        end repeat
    end tell
    return "message not found"
end run

on joinLines(itemsList)
    set oldDelimiters to AppleScript's text item delimiters
    set AppleScript's text item delimiters to linefeed
    set joinedText to itemsList as text
    set AppleScript's text item delimiters to oldDelimiters
    return joinedText
end joinLines
"#;
    let output = run_osascript(script, &[&message_id.to_string(), mailbox_scope]).await?;
    Ok(
        if matches!(output.as_str(), "no attachments" | "message not found") {
            output
        } else {
            format!("index\tname\tsize_bytes\tdownloaded\n{output}")
        },
    )
}

pub async fn save_attachment(
    message_id: u64,
    mailbox_scope: &str,
    attachment_index: usize,
    destination: &Path,
    overwrite: bool,
) -> Result<String> {
    validate_mailbox_scope(mailbox_scope)?;
    if attachment_index == 0 {
        bail!("attachment_index must be 1 or greater");
    }
    validate_save_destination(destination, overwrite).await?;
    let temporary_destination = temporary_save_path(destination)?;
    let script = r#"
on run argv
    set targetId to (item 1 of argv) as integer
    set mailboxScope to item 2 of argv
    set attachmentIndex to (item 3 of argv) as integer
    set destinationPath to item 4 of argv
    tell application "Mail"
        if mailboxScope is "inbox" then
            set sourceMailbox to inbox
        else if mailboxScope is "trash" then
            set sourceMailbox to trash mailbox
        else if mailboxScope is "junk" then
            set sourceMailbox to junk mailbox
        else if mailboxScope is "sent" then
            set sourceMailbox to sent mailbox
        else
            set sourceMailbox to drafts mailbox
        end if
        repeat with messageItem in messages of sourceMailbox
            if (id of messageItem) is targetId then
                set attachmentItems to mail attachments of messageItem
                if attachmentIndex is greater than (count of attachmentItems) then error "Attachment index is out of range."
                set attachmentItem to item attachmentIndex of attachmentItems
                save attachmentItem in (POSIX file destinationPath)
                return name of attachmentItem
            end if
        end repeat
    end tell
    error "Message not found in the selected mailbox."
end run
"#;
    let attachment_index = attachment_index.to_string();
    let destination_arg = temporary_destination.to_string_lossy().into_owned();
    let save_result = run_osascript(
        script,
        &[
            &message_id.to_string(),
            mailbox_scope,
            &attachment_index,
            &destination_arg,
        ],
    )
    .await;
    let saved_name = match save_result {
        Ok(name) => name,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary_destination).await;
            return Err(error);
        }
    };
    let metadata = match tokio::fs::metadata(&temporary_destination).await {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary_destination).await;
            return Err(error).with_context(|| {
                format!(
                    "Mail reported success but did not save {}",
                    temporary_destination.display()
                )
            });
        }
    };
    if !metadata.is_file() {
        let _ = tokio::fs::remove_file(&temporary_destination).await;
        bail!(
            "Mail did not save a regular file at {}",
            temporary_destination.display()
        );
    }
    if let Err(error) = tokio::fs::rename(&temporary_destination, destination).await {
        let _ = tokio::fs::remove_file(&temporary_destination).await;
        return Err(error).with_context(|| {
            format!(
                "cannot move saved attachment from {} to {}",
                temporary_destination.display(),
                destination.display()
            )
        });
    }
    let metadata = tokio::fs::metadata(destination).await.with_context(|| {
        format!(
            "Mail reported success but did not save {}",
            destination.display()
        )
    })?;
    Ok(format!(
        "status: complete\nattachment: {saved_name}\npath: {}\nbytes: {}",
        destination.display(),
        metadata.len()
    ))
}

pub async fn send(to: &str, subject: &str, body: &str, attachments: &[PathBuf]) -> Result<String> {
    for attachment in attachments {
        validate_attachment(attachment).await?;
    }
    // An empty preferred sender lets the AppleScript select the first enabled
    // Apple Mail account. No personal address is baked into the binary.
    let preferred_sender = env::var("FINN_MAIL_SENDER")
        .map(|value| value.trim().to_owned())
        .unwrap_or_default();
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
    let reported_sender = if preferred_sender.is_empty() {
        "first enabled Apple Mail account"
    } else {
        preferred_sender.as_str()
    };
    Ok(format!(
        "status: accepted_by_apple_mail\nto: {to}\nsubject: {subject}\nsender: {reported_sender}\nattachments: {}",
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

fn validate_mailbox_scope(scope: &str) -> Result<()> {
    if matches!(scope, "inbox" | "trash" | "junk" | "sent" | "drafts") {
        Ok(())
    } else {
        bail!("unsupported mailbox '{scope}'; expected inbox, trash, junk, sent, or drafts")
    }
}

async fn validate_save_destination(path: &Path, overwrite: bool) -> Result<()> {
    let parent = path
        .parent()
        .context("attachment destination must have a parent directory")?;
    let parent_metadata = tokio::fs::metadata(parent).await.with_context(|| {
        format!(
            "attachment destination directory does not exist: {}",
            parent.display()
        )
    })?;
    if !parent_metadata.is_dir() {
        bail!(
            "attachment destination parent is not a directory: {}",
            parent.display()
        );
    }
    match tokio::fs::metadata(path).await {
        Ok(metadata) if metadata.is_dir() => {
            bail!("attachment destination is a directory: {}", path.display())
        }
        Ok(_) if !overwrite => {
            bail!(
                "attachment destination exists and overwrite is false: {}",
                path.display()
            )
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("cannot inspect {}", path.display()));
        }
    }
    Ok(())
}

fn temporary_save_path(destination: &Path) -> Result<PathBuf> {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("attachment destination must have a valid file name")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(destination.with_file_name(format!(
        ".{file_name}.finn-{}-{nonce}.tmp",
        std::process::id()
    )))
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

    #[tokio::test]
    async fn validates_attachment_save_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("invoice.pdf");

        validate_save_destination(&destination, false)
            .await
            .unwrap();
        tokio::fs::write(&destination, b"old").await.unwrap();
        assert!(
            validate_save_destination(&destination, false)
                .await
                .is_err()
        );
        validate_save_destination(&destination, true).await.unwrap();
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"old");

        let missing_parent = temp.path().join("missing").join("invoice.pdf");
        assert!(
            validate_save_destination(&missing_parent, false)
                .await
                .is_err()
        );

        let temporary = temporary_save_path(&destination).unwrap();
        assert_eq!(temporary.parent(), destination.parent());
        assert_ne!(temporary, destination);
    }

    #[test]
    fn validates_mailbox_scopes() {
        for scope in ["inbox", "trash", "junk", "sent", "drafts"] {
            assert!(validate_mailbox_scope(scope).is_ok());
        }
        assert!(validate_mailbox_scope("all").is_err());
        assert!(validate_mailbox_scope("INBOX").is_err());
    }
}
