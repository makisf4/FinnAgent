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
const RECENT_ATTACHMENT_SCAN_LIMIT: usize = 2_000;
const DATED_ATTACHMENT_SCAN_LIMIT: usize = 10_000;
const RECENT_ATTACHMENT_CANDIDATE_LIMIT: usize = 25;

pub async fn search(query: &str, mailbox_scope: &str, limit: usize) -> Result<String> {
    validate_mailbox_scope(mailbox_scope)?;
    let script = r#"
on run argv
    set queryText to item 1 of argv
    set resultLimit to (item 2 of argv) as integer
    set mailboxScope to item 3 of argv
    set scanLimit to (item 4 of argv) as integer
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
            set allMessages to messages of sourceMailbox
            set scanCount to count of allMessages
            if scanCount is greater than scanLimit then set scanCount to scanLimit
            if scanCount is 0 then return ""
            set messageItems to items 1 through scanCount of allMessages
            repeat with messageItem in messageItems
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
        &[
            query,
            &limit.clamp(1, 100).to_string(),
            mailbox_scope,
            &RECENT_ATTACHMENT_SCAN_LIMIT.to_string(),
        ],
    )
    .await?;
    Ok(if output.trim().is_empty() {
        format!("no matching messages in {mailbox_scope}")
    } else {
        format!(
            "mailbox: {mailbox_scope}\nscan: newest-first, at most {RECENT_ATTACHMENT_SCAN_LIMIT} messages\nid\tsender\tsubject\tdate\tattachments\n{output}"
        )
    })
}

pub async fn recent_attachments(
    query: &str,
    extension: &str,
    mailbox_scope: &str,
    limit: usize,
    after_date: &str,
) -> Result<String> {
    validate_mailbox_scope(mailbox_scope)?;
    validate_attachment_extension(extension)?;
    let cutoff = parse_after_date(after_date)?;
    let invoice_query = invoice_like_query(query).to_string();
    let strict_query = query.contains('@').to_string();
    let (cutoff_enabled, cutoff_year, cutoff_month, cutoff_day) = cutoff
        .map(|(year, month, day)| (true, year, month, day))
        .unwrap_or((false, 0, 0, 0));
    let scan_limit = if cutoff_enabled {
        DATED_ATTACHMENT_SCAN_LIMIT
    } else {
        RECENT_ATTACHMENT_SCAN_LIMIT
    };
    let script = r#"
on run argv
    set queryText to item 1 of argv
    set extensionText to item 2 of argv
    set mailboxScope to item 3 of argv
    set resultLimit to (item 4 of argv) as integer
    set scanLimit to (item 5 of argv) as integer
    set candidateLimit to (item 6 of argv) as integer
    set invoiceQuery to (item 7 of argv) is "true"
    set strictQuery to (item 8 of argv) is "true"
    set cutoffEnabled to (item 9 of argv) is "true"
    set cutoffYear to (item 10 of argv) as integer
    set cutoffMonth to (item 11 of argv) as integer
    set cutoffDay to (item 12 of argv) as integer
    set matchingLines to {}
    set fallbackLines to {}
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

            set allMessages to messages of sourceMailbox
            if cutoffEnabled then
                set cutoffDate to current date
                set year of cutoffDate to cutoffYear
                set month of cutoffDate to cutoffMonth
                set day of cutoffDate to cutoffDay
                set time of cutoffDate to 0
            end if
            set scanCount to count of allMessages
            if scanCount is greater than scanLimit then set scanCount to scanLimit
            if scanCount is 0 then return ""
            set messageItems to items 1 through scanCount of allMessages

            repeat with messageItem in messageItems
                set messageSubject to subject of messageItem
                set messageSender to sender of messageItem
                set messageDate to date received of messageItem
                if cutoffEnabled and messageDate < cutoffDate then exit repeat
                set messageId to id of messageItem
                set messageMatches to ((queryText is "") or (messageSubject contains queryText) or (messageSender contains queryText))
                if invoiceQuery and ((messageSubject contains "invoice") or (messageSubject contains "τιμολ") or (messageSubject contains "απυ") or (messageSubject contains "receipt") or (messageSubject contains "παραστατ") or (messageSubject contains "απόδειξ") or (messageSubject contains "αποδείξ")) then set messageMatches to true
                if (strictQuery is false) or messageMatches then
                    set attachmentItems to mail attachments of messageItem
                    repeat with attachmentIndex from 1 to count of attachmentItems
                        set attachmentItem to item attachmentIndex of attachmentItems
                        set attachmentName to name of attachmentItem
                        set extensionMatches to ((extensionText is "any") or (attachmentName ends with ("." & extensionText)))
                        set queryMatches to (messageMatches or (attachmentName contains queryText))
                        if invoiceQuery and ((attachmentName contains "invoice") or (attachmentName contains "τιμολ") or (attachmentName contains "απυ") or (attachmentName contains "receipt") or (attachmentName contains "παραστατ") or (attachmentName contains "απόδειξ") or (attachmentName contains "αποδείξ")) then set queryMatches to true
                        if extensionMatches then
                            set attachmentSize to 0
                            set isDownloaded to false
                            try
                                set attachmentSize to file size of attachmentItem
                            end try
                            try
                                set isDownloaded to downloaded of attachmentItem
                            end try
                            set rowText to (queryMatches as text) & tab & (messageId as text) & tab & messageSender & tab & messageSubject & tab & (messageDate as text) & tab & (attachmentIndex as text) & tab & attachmentName & tab & (attachmentSize as text) & tab & (isDownloaded as text)
                            if queryMatches then
                                set end of matchingLines to rowText
                            else
                                set end of fallbackLines to rowText
                            end if
                            if (count of matchingLines) is greater than or equal to resultLimit then exit repeat
                            if (strictQuery is false) and ((count of matchingLines) + (count of fallbackLines)) is greater than or equal to candidateLimit then exit repeat
                        end if
                    end repeat
                end if
                if (count of matchingLines) is greater than or equal to resultLimit then exit repeat
                if (strictQuery is false) and ((count of matchingLines) + (count of fallbackLines)) is greater than or equal to candidateLimit then exit repeat
            end repeat
        end tell
    end ignoring
    return joinLines(matchingLines & fallbackLines)
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
        &[
            query,
            extension,
            mailbox_scope,
            &limit.clamp(1, 20).to_string(),
            &scan_limit.to_string(),
            &RECENT_ATTACHMENT_CANDIDATE_LIMIT.to_string(),
            &invoice_query,
            &strict_query,
            &cutoff_enabled.to_string(),
            &cutoff_year.to_string(),
            &cutoff_month.to_string(),
            &cutoff_day.to_string(),
        ],
    )
    .await?;
    Ok(if output.trim().is_empty() {
        format!(
            "no matching {extension} attachments in the bounded newest-first scan of {mailbox_scope}"
        )
    } else {
        let candidate_count = output.lines().count();
        let query_match_count = output
            .lines()
            .filter(|line| line.starts_with("true\t"))
            .count();
        format!(
            "mailbox: {mailbox_scope}\nquery_matches: {query_match_count}\ncandidates: {candidate_count}\nafter_date: {}\nscan: newest-first, hard cap {scan_limit} messages; email-address queries are strict and skip unrelated attachments\nquery_match\tmessage_id\tsender\tsubject\tdate\tattachment_index\tattachment_name\tsize_bytes\tdownloaded\n{output}",
            if after_date.is_empty() {
                "none"
            } else {
                after_date
            }
        )
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
        set matchingMessages to (every message of sourceMailbox whose id is targetId)
        if (count of matchingMessages) is 0 then return "message not found"
        set messageItem to item 1 of matchingMessages
        return "from: " & (sender of messageItem) & linefeed & "subject: " & (subject of messageItem) & linefeed & "date: " & ((date received of messageItem) as text) & linefeed & linefeed & (content of messageItem)
    end tell
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
        set matchingMessages to (every message of sourceMailbox whose id is targetId)
        if (count of matchingMessages) is 0 then return "message not found"
        set messageItem to item 1 of matchingMessages
        set attachmentItems to mail attachments of messageItem
        if (count of attachmentItems) is 0 then return "no attachments"
        set outputLines to {}
        repeat with attachmentIndex from 1 to count of attachmentItems
            set attachmentItem to item attachmentIndex of attachmentItems
            set end of outputLines to (attachmentIndex as text) & tab & (name of attachmentItem) & tab & ((file size of attachmentItem) as text) & tab & ((downloaded of attachmentItem) as text)
        end repeat
        return my joinLines(outputLines)
    end tell
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
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary_destination)
        .await
        .with_context(|| {
            format!(
                "cannot create Mail attachment staging file {}",
                temporary_destination.display()
            )
        })?;
    let script = r#"
on run argv
    set targetId to (item 1 of argv) as integer
    set mailboxScope to item 2 of argv
    set attachmentIndex to (item 3 of argv) as integer
    set destinationPath to item 4 of argv
    set destinationAlias to (POSIX file destinationPath) as alias
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
        set matchingMessages to (every message of sourceMailbox whose id is targetId)
        if (count of matchingMessages) is 0 then error "Message not found in the selected mailbox."
        set messageItem to item 1 of matchingMessages
        set attachmentItems to mail attachments of messageItem
        if attachmentIndex is greater than (count of attachmentItems) then error "Attachment index is out of range."
        set attachmentItem to item attachmentIndex of attachmentItems
        save attachmentItem in destinationAlias
        return name of attachmentItem
    end tell
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

pub fn unique_destination(destination: &Path) -> PathBuf {
    if !destination.exists() {
        return destination.to_path_buf();
    }
    let stem = destination
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let extension = destination
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    for suffix in 1_u32.. {
        let candidate = parent.join(format!("{stem} ({suffix}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
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

fn validate_attachment_extension(extension: &str) -> Result<()> {
    if matches!(
        extension,
        "any" | "pdf" | "doc" | "docx" | "xls" | "xlsx" | "csv" | "png" | "jpg" | "jpeg"
    ) {
        Ok(())
    } else {
        bail!("unsupported attachment extension '{extension}'")
    }
}

fn parse_after_date(value: &str) -> Result<Option<(i32, u32, u32)>> {
    if value.is_empty() {
        return Ok(None);
    }
    let mut parts = value.split('-');
    let year = parts
        .next()
        .and_then(|part| part.parse::<i32>().ok())
        .context("after_date must use YYYY-MM-DD")?;
    let month = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .context("after_date must use YYYY-MM-DD")?;
    let day = parts
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .context("after_date must use YYYY-MM-DD")?;
    if parts.next().is_some() || !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        bail!("after_date must be a valid YYYY-MM-DD date");
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day == 0 || day > max_day {
        bail!("after_date must be a valid YYYY-MM-DD date");
    }
    Ok(Some((year, month, day)))
}

fn invoice_like_query(query: &str) -> bool {
    let query = query.to_lowercase();
    [
        "invoice",
        "receipt",
        "τιμολ",
        "απυ",
        "παραστατ",
        "απόδειξ",
        "αποδείξ",
        "αποδειξ",
    ]
    .iter()
    .any(|keyword| query.contains(keyword))
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
    let stem = destination
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(file_name);
    let extension = destination
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(PathBuf::from("/private/tmp").join(format!(
        "{stem}.finn-{}-{nonce}.tmp{extension}",
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
        .map_err(|_| {
            anyhow::anyhow!(
                "MAIL_TIMEOUT: Apple Mail did not respond within {} seconds",
                MAIL_TIMEOUT.as_secs()
            )
        })??;
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
        assert_eq!(temporary.parent(), Some(Path::new("/private/tmp")));
        assert_ne!(temporary, destination);
        assert_eq!(
            temporary.extension().and_then(|value| value.to_str()),
            Some("pdf")
        );
        assert!(
            !temporary
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with('.')
        );
    }

    #[tokio::test]
    async fn chooses_unique_attachment_destination_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("invoice.pdf");
        tokio::fs::write(&destination, b"original").await.unwrap();
        tokio::fs::write(temp.path().join("invoice (1).pdf"), b"first")
            .await
            .unwrap();
        assert_eq!(
            unique_destination(&destination),
            temp.path().join("invoice (2).pdf")
        );
        assert_eq!(
            unique_destination(&temp.path().join("new.pdf")),
            temp.path().join("new.pdf")
        );
    }

    #[test]
    fn validates_mailbox_scopes() {
        for scope in ["inbox", "trash", "junk", "sent", "drafts"] {
            assert!(validate_mailbox_scope(scope).is_ok());
        }
        assert!(validate_mailbox_scope("all").is_err());
        assert!(validate_mailbox_scope("INBOX").is_err());
    }

    #[test]
    fn validates_recent_attachment_extensions() {
        for extension in ["any", "pdf", "docx", "xlsx", "csv", "png", "jpg", "jpeg"] {
            assert!(validate_attachment_extension(extension).is_ok());
        }
        assert!(validate_attachment_extension("PDF").is_err());
        assert!(validate_attachment_extension("zip").is_err());
    }

    #[test]
    fn recognizes_invoice_queries_in_english_and_greek() {
        for query in [
            "invoice",
            "latest invoices",
            "τιμολόγια",
            "ΑΠΥ",
            "αποδείξεις",
        ] {
            assert!(invoice_like_query(query));
        }
        assert!(!invoice_like_query("project report"));
    }

    #[test]
    fn validates_optional_after_dates() {
        assert_eq!(parse_after_date("").unwrap(), None);
        assert_eq!(parse_after_date("2026-01-01").unwrap(), Some((2026, 1, 1)));
        assert_eq!(parse_after_date("2024-02-29").unwrap(), Some((2024, 2, 29)));
        for invalid in ["01/01/2026", "2026-02-29", "2026-13-01", "2026-01-00"] {
            assert!(parse_after_date(invalid).is_err());
        }
    }

    #[tokio::test]
    #[ignore = "requires a configured Apple Mail account and Automation permission"]
    async fn finds_recent_pdf_attachments_in_live_mail() {
        let result = recent_attachments("τιμολόγια", "pdf", "inbox", 5, "")
            .await
            .unwrap();
        assert!(result.contains("query_match\tmessage_id\tsender\tsubject\tdate"));
    }

    #[tokio::test]
    #[ignore = "requires a configured Apple Mail account and Automation permission"]
    async fn reads_and_lists_a_recent_live_message_by_id() {
        let recent = recent_attachments("", "pdf", "inbox", 1, "").await.unwrap();
        let row = recent
            .lines()
            .find(|line| line.starts_with("true\t") || line.starts_with("false\t"))
            .expect("expected a recent PDF attachment row");
        let message_id = row
            .split('\t')
            .nth(1)
            .expect("expected message ID")
            .parse::<u64>()
            .expect("message ID should be numeric");

        let message = read(message_id, "inbox").await.unwrap();
        assert!(message.starts_with("from: "));
        let attachments = list_attachments(message_id, "inbox").await.unwrap();
        assert!(attachments.starts_with("index\tname\tsize_bytes\tdownloaded"));
    }

    #[tokio::test]
    #[ignore = "requires a configured Apple Mail account and Automation permission"]
    async fn searches_live_pdf_attachments_by_sender_and_cutoff_date() {
        let result = recent_attachments("nsimeonakis@gmail.com", "pdf", "inbox", 20, "2026-01-01")
            .await
            .unwrap();
        assert!(result.starts_with("mailbox:"), "expected matching PDFs");
    }

    #[tokio::test]
    #[ignore = "requires Apple Mail Automation and writes one temporary Desktop attachment"]
    async fn saves_a_live_attachment_through_visible_staging_path() {
        let recent = recent_attachments("", "pdf", "inbox", 1, "").await.unwrap();
        let row = recent
            .lines()
            .find(|line| line.starts_with("true\t") || line.starts_with("false\t"))
            .expect("expected a recent PDF attachment row");
        let fields = row.split('\t').collect::<Vec<_>>();
        let message_id = fields[1].parse::<u64>().unwrap();
        let attachment_index = fields[5].parse::<usize>().unwrap();
        let home = PathBuf::from(env::var("HOME").unwrap());
        let smoke_dir = home.join("Desktop").join(format!(
            "FinnMailSaveSmoke-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        ));
        tokio::fs::create_dir(&smoke_dir).await.unwrap();
        let destination = smoke_dir.join("attachment-smoke-test.pdf");

        let result =
            save_attachment(message_id, "inbox", attachment_index, &destination, false).await;
        let _ = tokio::fs::remove_file(&destination).await;
        let _ = tokio::fs::remove_dir(&smoke_dir).await;
        assert!(result.is_ok(), "{result:?}");
    }
}
