//! Deterministic task authorization derived from the user's request text.
//!
//! `TaskAuthorization` is computed once per task from the raw natural-language
//! request (English and Greek) and gates every tool exposure and execution.
//! Execution-time checks in `ToolContext` remain mandatory because untrusted
//! data can enter the conversation after a tool schema was already exposed.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TaskAuthorization {
    allow_mail_send: bool,
    allow_trash: bool,
    allow_mail_attachment_save: bool,
    allow_codex: bool,
    allow_web: bool,
    allow_web_download: bool,
    allow_file_write: bool,
    allow_directory_create: bool,
    allow_artifact_write: bool,
    allow_overwrite: bool,
    allow_file_read: bool,
    allow_file_content_read: bool,
    allow_mail_read: bool,
    allow_system_info: bool,
    authorized_recipient_hashes: [u64; 4],
    authorized_recipient_count: u8,
    authorized_attachment_hashes: [u64; 8],
    authorized_attachment_count: u8,
    authorized_target_hashes: [u64; 16],
    authorized_target_count: u8,
    authorized_location_flags: u8,
    untrusted_context: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CapabilitySet {
    mail_send: bool,
    trash: bool,
    mail_attachment_save: bool,
    codex: bool,
    web: bool,
    web_download: bool,
    file_write: bool,
    directory_create: bool,
    artifact_write: bool,
    overwrite: bool,
    file_read: bool,
    file_content_read: bool,
    mail_read: bool,
    system_info: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AuthorizationBindings {
    recipient_hashes: [u64; 4],
    recipient_count: u8,
    attachment_hashes: [u64; 8],
    attachment_count: u8,
    target_hashes: [u64; 16],
    target_count: u8,
    location_flags: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ParsedIntent {
    capabilities: CapabilitySet,
    bindings: AuthorizationBindings,
}

impl TaskAuthorization {
    pub fn from_task(task: &str) -> Self {
        let intent = ParsedIntent::parse(task);
        let capabilities = intent.capabilities;
        let bindings = intent.bindings;
        Self {
            allow_mail_send: capabilities.mail_send,
            allow_trash: capabilities.trash,
            allow_mail_attachment_save: capabilities.mail_attachment_save,
            allow_codex: capabilities.codex,
            allow_web: capabilities.web,
            allow_web_download: capabilities.web_download,
            allow_file_write: capabilities.file_write,
            allow_directory_create: capabilities.directory_create,
            allow_artifact_write: capabilities.artifact_write,
            allow_overwrite: capabilities.overwrite,
            allow_file_read: capabilities.file_read,
            allow_file_content_read: capabilities.file_content_read,
            allow_mail_read: capabilities.mail_read,
            allow_system_info: capabilities.system_info,
            authorized_recipient_hashes: bindings.recipient_hashes,
            authorized_recipient_count: bindings.recipient_count,
            authorized_attachment_hashes: bindings.attachment_hashes,
            authorized_attachment_count: bindings.attachment_count,
            authorized_target_hashes: bindings.target_hashes,
            authorized_target_count: bindings.target_count,
            authorized_location_flags: bindings.location_flags,
            untrusted_context: false,
        }
    }

    pub fn with_untrusted_context(mut self, active: bool) -> Self {
        self.untrusted_context = active;
        self
    }

    pub fn mark_untrusted(&mut self) {
        self.untrusted_context = true;
    }

    pub fn untrusted_context_active(&self) -> bool {
        self.untrusted_context
    }

    pub fn audit_snapshot(&self, include_server_web: bool) -> Value {
        let exposed_tools = super::definitions_for_turn(*self, include_server_web)
            .into_iter()
            .filter_map(|tool| {
                tool.get("name")
                    .or_else(|| tool.get("type"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        json!({
            "source": "current_user_task",
            "untrusted_context": self.untrusted_context,
            "capabilities": {
                "mail_send": self.allow_mail_send,
                "trash": self.allow_trash,
                "mail_attachment_save": self.allow_mail_attachment_save,
                "codex": self.allow_codex,
                "web": self.allow_web,
                "web_download": self.allow_web_download,
                "file_write": self.allow_file_write,
                "directory_create": self.allow_directory_create,
                "artifact_write": self.allow_artifact_write,
                "overwrite": self.allow_overwrite,
                "file_read": self.allow_file_read,
                "file_content_read": self.allow_file_content_read,
                "mail_read": self.allow_mail_read,
                "system_info": self.allow_system_info,
            },
            "bindings": {
                "recipients": self.authorized_recipient_count,
                "attachments": self.authorized_attachment_count,
                "targets": self.authorized_target_count,
                "locations": location_flag_names(self.authorized_location_flags),
            },
            "exposed_tools": exposed_tools,
        })
    }

    /// True when the current task authorizes OpenRouter server-side web tools.
    pub(super) fn server_web_allowed(self) -> bool {
        self.allow_web
    }

    pub(super) fn require_tool(self, name: &str) -> Result<()> {
        match name {
            "mail_send" => self.require_mail_send(),
            "move_to_trash" => self.require_trash(),
            "mail_save_attachment" if !self.allow_mail_attachment_save => {
                bail!(
                    "mail_save_attachment denied: the user did not explicitly ask to save, copy, download, move, or extract an attachment"
                )
            }
            "mail_search" | "mail_read" | "mail_list_attachments" if self.allow_mail_read => Ok(()),
            "codex_start" | "codex_resume" if self.allow_codex => Ok(()),
            "path_status" | "list_directory" | "find_files" | "find_large_files"
                if self.allow_file_read =>
            {
                Ok(())
            }
            "read_file" | "artifact_read"
                if self.allow_file_content_read
                    && (!self.untrusted_context || self.authorized_attachment_count > 0) =>
            {
                Ok(())
            }
            "run_shell" => bail!(
                "run_shell is unavailable; use Finn's dedicated tools or explicitly request Codex delegation"
            ),
            "system_info" if self.allow_system_info => Ok(()),
            "download_url" if self.allow_web_download => Ok(()),
            "write_file" if self.allow_file_write => Ok(()),
            "create_directory" if self.allow_directory_create => Ok(()),
            "document_create"
            | "document_replace_text"
            | "spreadsheet_update"
            | "pdf_replace_text"
            | "pdf_transform_pages"
            | "image_transform"
                if self.allow_artifact_write =>
            {
                Ok(())
            }
            "mail_save_attachment" => Ok(()),
            _ if self.untrusted_context => bail!(
                "{name} denied: untrusted external data is active and the user's current task did not explicitly authorize this capability"
            ),
            _ => bail!(
                "{name} denied: the user's current task did not explicitly authorize this capability"
            ),
        }
    }

    pub(super) fn require_overwrite(self, overwrite: bool) -> Result<()> {
        if overwrite && !self.allow_overwrite {
            bail!(
                "overwrite denied: the original user task did not explicitly authorize replacing an existing file"
            );
        }
        Ok(())
    }

    pub(super) fn require_mail_recipient(self, recipient: &str) -> Result<()> {
        let hash = stable_text_hash(&recipient.trim().to_ascii_lowercase());
        if self.authorized_recipient_hashes[..self.authorized_recipient_count as usize]
            .contains(&hash)
        {
            Ok(())
        } else {
            bail!(
                "mail_send denied: the recipient must be an explicit email address in the user's current task"
            )
        }
    }

    pub(super) fn require_outbound_attachments(self, attachments: &[PathBuf]) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        let authorized =
            &self.authorized_attachment_hashes[..self.authorized_attachment_count as usize];
        for attachment in attachments {
            let Some(name) = attachment.file_name().and_then(|value| value.to_str()) else {
                bail!("mail_send attachment denied: attachment path has no valid file name");
            };
            if !authorized.contains(&stable_text_hash(&name.to_ascii_lowercase())) {
                bail!(
                    "mail_send attachment denied: every attachment file name must be explicit in the user's current task"
                );
            }
        }
        Ok(())
    }

    pub(super) fn require_trash_path(self, path: &Path) -> Result<()> {
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            bail!("move_to_trash denied: target path has no valid file name");
        };
        let hash = stable_text_hash(&name.to_ascii_lowercase());
        if self.authorized_target_hashes[..self.authorized_target_count as usize].contains(&hash) {
            Ok(())
        } else {
            bail!(
                "move_to_trash denied: the target filename or path must be explicit in the user's current task"
            )
        }
    }

    pub(super) fn require_read_path(self, path: &Path, home: &Path, content: bool) -> Result<()> {
        if !self.untrusted_context {
            return Ok(());
        }
        let authorized_files =
            &self.authorized_attachment_hashes[..self.authorized_attachment_count as usize];
        let file_matches = path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| {
                authorized_files.contains(&stable_text_hash(&name.to_ascii_lowercase()))
            });
        if file_matches {
            return Ok(());
        }
        if content {
            bail!(
                "file content read denied: after reading untrusted mail, the exact file name must appear in the user's current task"
            );
        }
        if self.path_location_allowed(path, home) {
            Ok(())
        } else {
            bail!(
                "filesystem read denied: the user's current task did not name this file or location"
            )
        }
    }

    pub(super) fn require_write_path(self, path: &Path, home: &Path) -> Result<()> {
        if !self.untrusted_context {
            return Ok(());
        }
        let authorized_files =
            &self.authorized_attachment_hashes[..self.authorized_attachment_count as usize];
        let file_matches = path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| {
                authorized_files.contains(&stable_text_hash(&name.to_ascii_lowercase()))
            });
        if file_matches || self.path_location_allowed(path, home) {
            Ok(())
        } else {
            bail!(
                "write path denied: after reading untrusted mail, the user's current task must name the destination file or Desktop/Documents/Downloads location"
            )
        }
    }

    fn path_location_allowed(self, path: &Path, home: &Path) -> bool {
        let relative = path.strip_prefix(home).unwrap_or(path);
        let first = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .unwrap_or_default();
        match first {
            "Desktop" => self.authorized_location_flags & LOCATION_DESKTOP != 0,
            "Documents" => self.authorized_location_flags & LOCATION_DOCUMENTS != 0,
            "Downloads" => self.authorized_location_flags & LOCATION_DOWNLOADS != 0,
            _ => false,
        }
    }

    pub(super) fn require_mail_send(self) -> Result<()> {
        if !self.allow_mail_send {
            bail!("mail_send denied: the original user task did not explicitly authorize email");
        }
        Ok(())
    }

    pub(super) fn require_trash(self) -> Result<()> {
        if !self.allow_trash {
            bail!(
                "move_to_trash denied: the original user task did not explicitly authorize deletion"
            );
        }
        Ok(())
    }
}

impl ParsedIntent {
    fn parse(task: &str) -> Self {
        let text = TaskText::new(task);
        let raw = text.raw();

        let mail_object = text
            .has_phrase(&["email", "emails", "mail", "mails", "message", "messages"])
            || text.has_stem(&["μήνυμ", "μηνυμ"]);
        let send_action = text.has_phrase(&[
            "send",
            "forward",
            "reply",
            "email me",
            "email it",
            "email this",
            "email that",
            "email the",
            "email a",
            "mail me",
            "mail it",
            "mail this",
            "mail that",
            "mail the",
            "mail a",
        ]) || text.has_stem(&[
            "στείλ",
            "στειλ",
            "προώθησ",
            "προωθησ",
            "απάντησ",
            "απαντησ",
            "αποστολ",
        ]) || text.starts_with_word("email")
            || text.starts_with_word("mail");
        let send_negated = text.has_phrase(&[
            "do not send",
            "don't send",
            "dont send",
            "without sending",
            "never send",
            "do not forward",
            "don't forward",
            "dont forward",
            "do not reply",
            "don't reply",
            "dont reply",
        ]) || text.has_phrase(&["μη"]) && text.has_stem(&["στείλ", "στειλ"])
            || text.has_phrase(&["χωρίς", "χωρις"]) && text.has_stem(&["αποστολ"]);
        let transfer_action = text.has_phrase(&[
            "save", "copy", "download", "extract", "move", "put", "store", "βάλε", "βαλε",
        ]) || text.has_stem(&[
            "αποθήκευσ",
            "αποθηκευσ",
            "αντιγρα",
            "κατέβασ",
            "κατεβασ",
            "μετακίνησ",
            "μετακινησ",
        ]);
        let attachment_reference = text.has_phrase(&["attachment", "attachments", "attached"])
            || text.has_stem(&["συνημμ", "επισυναπτ"]);
        // Deictic references ("it", "that", "this") only imply an attachment
        // when the task also mentions mail; otherwise they collide constantly.
        let deictic_reference = text.has_phrase(&["it", "that", "this"]);
        let read_action = text.has_phrase(&[
            "read", "search", "find", "check", "look", "show", "list", "inspect", "βρες",
        ]) || text.has_stem(&[
            "summar",
            "analy",
            "διάβασ",
            "διαβασ",
            "ψάξ",
            "ψαξ",
            "δείξ",
            "δειξ",
            "έλεγξ",
            "ελεγξ",
        ]);
        let explicit_trash_action = text.has_phrase(&[
            "move to trash",
            "move it to trash",
            "move that to trash",
            "move this to trash",
            "to trash",
            "to the trash",
            "trash the",
            "trash this",
            "trash that",
            "into trash",
            "into the trash",
            "βάλε στον κάδο",
            "βαλε στον καδο",
            "μετακίνησε στον κάδο",
            "μετακινησε στον καδο",
        ]);
        let delete_action =
            text.has_phrase(&["delete", "remove"]) || text.has_stem(&["διαγρα", "σβήσ", "σβησ"]);
        let delete_negated = text.has_phrase(&[
            "do not delete",
            "don't delete",
            "dont delete",
            "do not remove",
            "don't remove",
            "dont remove",
            "without deleting",
        ]) || text.has_phrase(&["μη"])
            && text.has_stem(&["διαγρα", "σβήσ", "σβησ"]);
        let delete_negated = delete_negated
            || (delete_action && text.has_phrase(&["do not", "don't", "dont", "never", "without"]));
        let artifact_suboperation = artifact_page_or_image_suboperation(&text);
        let filesystem_target = text.has_phrase(&[
            "file",
            "files",
            "folder",
            "folders",
            "directory",
            "directories",
            "path",
            "desktop",
            "documents",
            "downloads",
        ]) || text.has_stem(&["αρχεί", "αρχει", "φάκελ", "φακελ"])
            || text.contains_fragment("~/")
            || text.contains_fragment("/users/")
            || text.has_file_extension();
        let codex_object = text.has_phrase(&["codex", "codex cli", "code cli"]);
        let codex_action = text.has_phrase(&[
            "use codex",
            "run codex",
            "start codex",
            "ask codex",
            "resume codex",
            "continue codex",
            "supervise codex",
            "control codex",
            "delegate to codex",
            "with codex",
            "use code cli",
            "run code cli",
        ]);
        let web_reference = text
            .has_phrase(&["web", "website", "webpage", "internet", "online", "url"])
            || text.contains_fragment("http://")
            || text.contains_fragment("https://");
        let web_action = text.has_phrase(&[
            "search",
            "browse",
            "look up",
            "research",
            "find online",
            "search online",
            "search the web",
            "fetch",
            "visit",
            "open the url",
            "read the url",
            "read the website",
        ]);
        let current_information = text.has_phrase(&[
            "latest",
            "current news",
            "recent news",
            "today's news",
            "todays news",
        ]);
        let write_verb = text.has_phrase(&["write", "create", "save", "make"])
            || text.has_stem(&["γραψ", "δημιουργησ", "φτιαξ", "βαλ"]);
        let file_noun = text.has_phrase(&[
            "file",
            "files",
            "report",
            "reports",
            "summary",
            "summaries",
            "note",
            "notes",
            "script",
            "scripts",
            "txt",
        ]) || text.has_stem(&["αρχε"])
            || text.has_file_extension();
        let file_write = write_verb && file_noun;
        let directory_create = ((text.has_phrase(&["create", "make"]))
            && text.has_phrase(&["folder", "directory"]))
            || (text.has_stem(&["δημιούργησ", "δημιουργησ", "φτιάξ", "φτιαξ"])
                && text.has_stem(&["φάκελ", "φακελ"]));
        let artifact_reference =
            text.has_phrase(&[
                "docx",
                "document",
                "documents",
                "pdf",
                "xlsx",
                "spreadsheet",
                "workbook",
                "image",
                "images",
                "phot",
                "photo",
                "photos",
                "png",
                "jpg",
                "jpeg",
                "gif",
                "webp",
                "tiff",
            ]) || text.has_stem(&["έγγρα", "εγγρα", "εικόν", "εικον", "φωτογραφ"]);
        let web_download = transfer_action
            && (artifact_reference
                || file_noun
                || text.has_phrase(&["from the web", "from the internet", "online"]));
        let artifact_action = text.has_phrase(&[
            "create",
            "edit",
            "update",
            "replace",
            "resize",
            "crop",
            "rotate",
            "convert",
            "grayscale",
        ]) || text.has_phrase(&["remove page", "remove pages"])
            || text.has_stem(&[
                "δημιούργησ",
                "δημιουργησ",
                "επεξεργ",
                "ενημέρωσ",
                "ενημερωσ",
                "αντικατάστ",
                "αντικαταστ",
                "περιστρ",
                "μετατροπ",
            ]);
        let conversational_mail_action = text.has_phrase(&["forward", "reply"])
            || text.has_stem(&["προώθησ", "προωθησ", "απάντησ", "απαντησ"]);
        let content_read_action = text.has_phrase(&[
            "read",
            "inspect",
            "open",
            "extract",
            "verify",
            "show content",
            "what is in",
            "what's in",
            "look at",
        ]) || text.has_stem(&[
            "summar",
            "analy",
            "διάβασ",
            "διαβασ",
            "περίληψ",
            "περιληψ",
            "ανάλυσ",
            "αναλυσ",
        ]);
        let artifact_write = artifact_reference && artifact_action;
        let overwrite = text.has_phrase(&["overwrite", "replace existing", "replace the existing"])
            || (text.has_phrase(&["αντικατάστησε", "αντικαταστησε"])
                && text.has_stem(&["υπάρχ", "υπαρχ"]))
            || (artifact_reference
                && (text.has_phrase(&["edit", "update", "replace"])
                    || text.has_stem(&[
                        "επεξεργ",
                        "ενημέρωσ",
                        "ενημερωσ",
                        "αντικατάστ",
                        "αντικαταστ",
                    ])));
        let overwrite = overwrite
            && !text.has_phrase(&[
                "do not overwrite",
                "don't overwrite",
                "dont overwrite",
                "without overwriting",
                "never overwrite",
            ])
            && !(text.has_phrase(&["do not", "don't", "dont", "never", "without"])
                && text.has_phrase(&["overwrite"]));
        let system_info = text.has_phrase(&[
            "system",
            "cpu",
            "processor",
            "memory",
            "ram",
            "disk",
            "storage",
            "hardware",
            "specs",
            "uptime",
            "machine",
        ]) || text.has_stem(&[
            "σύστημ",
            "συστημ",
            "μνήμ",
            "μνημ",
            "δίσκ",
            "δισκ",
            "επεξεργαστ",
        ]);

        let (recipient_hashes, recipient_count) = extract_recipient_hashes(raw);
        let (attachment_hashes, attachment_count) = extract_attachment_hashes(raw);
        let (target_hashes, target_count) = extract_target_hashes(raw);
        Self {
            capabilities: CapabilitySet {
                mail_send: send_action
                    && (mail_object || raw.contains('@') || conversational_mail_action)
                    && !send_negated
                    && recipient_count > 0,
                trash: (explicit_trash_action
                    || (delete_action && !artifact_suboperation && filesystem_target))
                    && !delete_negated,
                mail_attachment_save: transfer_action
                    && (attachment_reference || (mail_object && deictic_reference)),
                codex: codex_object && codex_action,
                web: (web_reference && web_action)
                    || web_download
                    || (current_information && text.has_phrase(&["search", "find", "look up"])),
                web_download,
                file_write,
                directory_create,
                artifact_write,
                overwrite,
                file_read: filesystem_target || artifact_reference,
                file_content_read: ((filesystem_target || artifact_reference)
                    && content_read_action)
                    || artifact_write,
                mail_read: (mail_object || attachment_reference)
                    && (read_action || transfer_action),
                system_info,
            },
            bindings: AuthorizationBindings {
                recipient_hashes,
                recipient_count,
                attachment_hashes,
                attachment_count,
                target_hashes,
                target_count,
                location_flags: location_flags(&text),
            },
        }
    }
}

/// Location bit flags used by `authorized_location_flags`.
const LOCATION_DESKTOP: u8 = 0b001;
const LOCATION_DOCUMENTS: u8 = 0b010;
const LOCATION_DOWNLOADS: u8 = 0b100;

fn location_flag_names(flags: u8) -> Vec<&'static str> {
    let mut names = Vec::new();
    if flags & LOCATION_DESKTOP != 0 {
        names.push("Desktop");
    }
    if flags & LOCATION_DOCUMENTS != 0 {
        names.push("Documents");
    }
    if flags & LOCATION_DOWNLOADS != 0 {
        names.push("Downloads");
    }
    names
}

/// Tokenized, normalized view of the user's task.
///
/// Authorization matching is word-boundary aware rather than raw substring
/// based, which prevents collisions such as "documents" matching inside an
/// unrelated word or " it " accidentally implying an attachment reference.
struct TaskText {
    /// Original lowercased task, used for character-level checks (e.g. `@`).
    raw: String,
    /// Lowercased task normalized to single spaces and padded with a leading
    /// and trailing space so `" phrase "` matches on word boundaries.
    padded: String,
    /// Whitespace-delimited lowercase tokens, punctuation stripped.
    tokens: Vec<String>,
}

impl TaskText {
    fn new(task: &str) -> Self {
        let raw = task.to_lowercase();
        let normalized = normalize_for_matching(task);
        // Split on any non-alphanumeric character except a few that appear
        // inside file names and paths, so tokens stay meaningful.
        let tokens: Vec<String> = normalized
            .split(|character: char| {
                !(character.is_alphanumeric() || matches!(character, '.' | '_' | '+' | '-' | '/'))
            })
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect();
        // Build a word-token stream for phrase matching: split on whitespace and
        // punctuation but keep alphanumerics together. Apostrophes are kept
        // inside words so contractions like "what's" and possessives like
        // "alex's" survive as single tokens and match apostrophe phrases.
        let words: Vec<&str> = normalized
            .split(|character: char| !(character.is_alphanumeric() || character == '\''))
            .map(|word| word.trim_matches('\''))
            .filter(|word| !word.is_empty())
            .collect();
        let padded = format!(" {} ", words.join(" "));
        Self {
            raw,
            padded,
            tokens,
        }
    }

    fn raw(&self) -> &str {
        &self.raw
    }

    /// True if any phrase appears on word boundaries. Phrases may contain
    /// multiple space-separated words.
    fn has_phrase(&self, phrases: &[&str]) -> bool {
        phrases
            .iter()
            .any(|phrase| self.padded.contains(&format!(" {phrase} ")))
    }

    /// True if any token starts with one of the given stems. Used for
    /// morphological matching (mainly Greek verb stems).
    fn has_stem(&self, stems: &[&str]) -> bool {
        self.tokens
            .iter()
            .any(|token| stems.iter().any(|stem| token.starts_with(stem)))
    }

    /// True if the first word of the task is exactly `word`.
    fn starts_with_word(&self, word: &str) -> bool {
        self.padded
            .trim_start()
            .split(' ')
            .next()
            .is_some_and(|first| first == word)
    }

    /// Raw substring check, reserved for path fragments like `~/` and `/users/`
    /// that intentionally are not word tokens.
    fn contains_fragment(&self, fragment: &str) -> bool {
        self.raw.contains(fragment)
    }

    /// True if any token ends with a supported artifact/file extension.
    fn has_file_extension(&self) -> bool {
        const EXTENSIONS: &[&str] = &[
            ".txt", ".docx", ".pdf", ".xlsx", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".tif",
            ".tiff",
        ];
        self.tokens.iter().any(|token| {
            EXTENSIONS
                .iter()
                .any(|extension| token.ends_with(extension))
        })
    }
}

/// Lowercases authorization input and removes Greek vowel diacritics.
///
/// Greek inflection frequently moves the accent within a word (for example,
/// `φάκελος` -> `φακέλους`). Authorization must not depend on where the user
/// placed that accent, and decomposed Unicode input should behave like
/// precomposed input.
fn normalize_for_matching(task: &str) -> String {
    task.to_lowercase()
        .chars()
        .filter_map(|character| match character {
            '\u{0300}'..='\u{036f}' => None,
            'ά' => Some('α'),
            'έ' => Some('ε'),
            'ή' => Some('η'),
            'ί' | 'ϊ' | 'ΐ' => Some('ι'),
            'ό' => Some('ο'),
            'ύ' | 'ϋ' | 'ΰ' => Some('υ'),
            'ώ' => Some('ω'),
            _ => Some(character),
        })
        .collect()
}

fn extract_recipient_hashes(task: &str) -> ([u64; 4], u8) {
    let mut hashes = [0_u64; 4];
    let mut count = 0_usize;
    for token in task.split(|character: char| {
        !(character.is_ascii_alphanumeric() || matches!(character, '@' | '.' | '_' | '+' | '-'))
    }) {
        let token = token
            .trim_matches(|character: char| matches!(character, '.' | '-' | '_'))
            .to_ascii_lowercase();
        let valid = token
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
        if valid && count < hashes.len() {
            hashes[count] = stable_text_hash(&token);
            count += 1;
        }
    }
    (hashes, count as u8)
}

fn extract_attachment_hashes(task: &str) -> ([u64; 8], u8) {
    let mut hashes = [0_u64; 8];
    let mut count = 0_usize;
    for token in task.split(|character: char| {
        !(character.is_alphanumeric() || matches!(character, '.' | '_' | '+' | '-'))
    }) {
        let name = token
            .trim_matches(|character: char| matches!(character, '.' | '-' | '_'))
            .to_ascii_lowercase();
        let supported = [
            ".txt", ".docx", ".pdf", ".xlsx", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".tif",
            ".tiff", ".csv", ".tsv", ".zip", ".md", ".json", ".xml", ".html", ".css", ".js", ".rs",
            ".py",
        ]
        .iter()
        .any(|extension| name.ends_with(extension));
        if supported && count < hashes.len() {
            hashes[count] = stable_text_hash(&name);
            count += 1;
        }
    }
    (hashes, count as u8)
}

fn extract_target_hashes(task: &str) -> ([u64; 16], u8) {
    let mut hashes = [0_u64; 16];
    let mut count = 0_usize;
    let mut add = |candidate: &str| {
        let candidate = candidate
            .trim_matches(|character: char| {
                character.is_whitespace()
                    || matches!(
                        character,
                        '"' | '\'' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
                    )
            })
            .trim_end_matches('/');
        let name = candidate.rsplit('/').next().unwrap_or(candidate).trim();
        if name.is_empty() || count >= hashes.len() {
            return;
        }
        let hash = stable_text_hash(&name.to_ascii_lowercase());
        if !hashes[..count].contains(&hash) {
            hashes[count] = hash;
            count += 1;
        }
    };

    for token in task.split_whitespace() {
        let trimmed = token.trim_matches(|character: char| {
            matches!(
                character,
                '"' | '\'' | ',' | ';' | ':' | '(' | ')' | '[' | ']'
            )
        });
        if trimmed.contains('/')
            || trimmed.rsplit('.').next().is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "txt"
                        | "docx"
                        | "pdf"
                        | "xlsx"
                        | "png"
                        | "jpg"
                        | "jpeg"
                        | "gif"
                        | "webp"
                        | "tif"
                        | "tiff"
                        | "csv"
                        | "tsv"
                        | "zip"
                        | "md"
                        | "json"
                        | "xml"
                        | "html"
                        | "css"
                        | "js"
                        | "rs"
                        | "py"
                )
            })
        {
            add(trimmed);
        }
    }

    let mut rest = task;
    while let Some(start) = rest.find('"') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('"') else {
            break;
        };
        add(&rest[..end]);
        rest = &rest[end + 1..];
    }
    (hashes, count as u8)
}

fn location_flags(text: &TaskText) -> u8 {
    let mut flags = 0_u8;
    if text.has_phrase(&["desktop"]) {
        flags |= LOCATION_DESKTOP;
    }
    if text.has_phrase(&["documents"]) {
        flags |= LOCATION_DOCUMENTS;
    }
    if text.has_phrase(&["downloads"]) {
        flags |= LOCATION_DOWNLOADS;
    }
    flags
}

fn stable_text_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn artifact_page_or_image_suboperation(text: &TaskText) -> bool {
    (text.has_phrase(&["pdf", "document", "docx"])
        && text.has_phrase(&["page", "pages", "text", "paragraph", "section"]))
        || (text.has_phrase(&["image", "photo", "png", "jpg", "jpeg"])
            && text.has_phrase(&["background", "color", "crop", "metadata"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capability probed by the authorization matrix.
    #[derive(Clone, Copy, Debug)]
    enum Cap {
        MailSend,
        Trash,
        AttachmentSave,
        FileWrite,
        DirCreate,
        ArtifactWrite,
        Overwrite,
        FileRead,
        FileContentRead,
        MailRead,
        SystemInfo,
        WebDownload,
    }

    impl Cap {
        fn granted(self, auth: &TaskAuthorization) -> bool {
            match self {
                Cap::MailSend => auth.allow_mail_send,
                Cap::Trash => auth.allow_trash,
                Cap::AttachmentSave => auth.allow_mail_attachment_save,
                Cap::FileWrite => auth.allow_file_write,
                Cap::DirCreate => auth.allow_directory_create,
                Cap::ArtifactWrite => auth.allow_artifact_write,
                Cap::Overwrite => auth.allow_overwrite,
                Cap::FileRead => auth.allow_file_read,
                Cap::FileContentRead => auth.allow_file_content_read,
                Cap::MailRead => auth.allow_mail_read,
                Cap::SystemInfo => auth.allow_system_info,
                Cap::WebDownload => auth.allow_web_download,
            }
        }
    }

    /// Table-driven authorization matrix. Each row lists a task, the
    /// capabilities that MUST be granted, and the capabilities that MUST be
    /// denied. Capabilities not listed in either column are unconstrained.
    #[test]
    fn authorization_matrix() {
        use Cap::*;
        let cases: &[(&str, &[Cap], &[Cap])] = &[
            // Pure questions grant nothing high impact.
            (
                "What can Finn do?",
                &[],
                &[
                    MailSend,
                    Trash,
                    FileWrite,
                    DirCreate,
                    ArtifactWrite,
                    MailRead,
                ],
            ),
            (
                "Does the folder named Makis exist on my Desktop?",
                &[FileRead],
                &[MailSend, Trash, FileWrite, DirCreate, FileContentRead],
            ),
            // Directory + file creation.
            (
                "Create a folder named Makis on my Desktop",
                &[DirCreate, FileRead],
                &[MailSend, Trash, FileWrite, ArtifactWrite],
            ),
            (
                "Write a zsh script on my Desktop that reports the ten largest files",
                &[FileWrite],
                &[MailSend, Trash, DirCreate],
            ),
            (
                "download o phot of larry bird on the Desktop",
                &[FileRead, WebDownload],
                &[MailSend, Trash],
            ),
            (
                "Create 12 folders on my Desktop named January through December. Inside each folder, create 7 empty TXT files named Monday.txt through Sunday.txt.",
                &[DirCreate, FileWrite, FileRead],
                &[MailSend, Trash],
            ),
            (
                "φτιάξε μου 12 φακέλους με τα ονόματα των μηνών στο Desktop και μέσα βάλε 7 txt με τα ονόματα των ημερών",
                &[DirCreate, FileWrite, FileRead],
                &[MailSend, Trash],
            ),
            (
                "χρησιμοποίησε bash και φτιάξε μου 12 φακέλους με τα ονόματα των μηνών στο Desktop και μέσα βάλε 7 txt με τα ονόματα των ημερών",
                &[DirCreate, FileWrite, FileRead],
                &[MailSend, Trash],
            ),
            // Deletion routes to Trash only for filesystem targets.
            ("Delete note.txt", &[Trash], &[MailSend]),
            ("Move that folder to Trash", &[Trash], &[MailSend]),
            (
                "Remove page 2 from report.pdf",
                &[ArtifactWrite],
                &[Trash, MailSend],
            ),
            ("Remove the attachment from the email", &[], &[Trash]),
            (
                "Read it but do not delete anything",
                &[],
                &[Trash, MailSend],
            ),
            (
                "Read the note but do not send email, delete files, or overwrite anything",
                &[FileRead],
                &[Trash, MailSend, Overwrite],
            ),
            // Mail reading vs sending.
            (
                "Find emails from example.com in my inbox",
                &[MailRead],
                &[MailSend, Trash, FileWrite],
            ),
            ("Read my email from Alex", &[MailRead], &[MailSend]),
            ("Please email the report to Alex", &[], &[MailSend, Trash]),
            ("Send \"hello\" to alex@example.com", &[MailSend], &[Trash]),
            ("Read it but do not send any email", &[], &[MailSend]),
            ("Forward it to Alex", &[], &[MailSend, Trash]),
            // Attachment save requires an attachment reference or mail deixis.
            (
                "Save the invoice attached to Alex's email in ~/Documents/Invoices",
                &[AttachmentSave],
                &[MailSend],
            ),
            // Artifact writes and overwrites.
            (
                "Read report.docx and create summary.docx in Documents",
                &[ArtifactWrite, FileRead, FileContentRead],
                &[MailSend, Trash],
            ),
            (
                "Create a DOCX report",
                &[ArtifactWrite],
                &[Overwrite, MailSend],
            ),
            (
                "Replace Revenue with Profit in report.docx",
                &[ArtifactWrite, Overwrite],
                &[MailSend],
            ),
            (
                "Create a DOCX report and overwrite the existing file",
                &[ArtifactWrite, Overwrite],
                &[MailSend],
            ),
            // General shell execution is unavailable, even when requested.
            (
                "Run a bash command to list processes",
                &[],
                &[MailSend, Trash],
            ),
            (
                "Summarize my Documents folder",
                &[FileRead],
                &[MailSend, Trash, FileWrite],
            ),
            // False-positive guards from the tokenized rewrite.
            // "commander" must not trip word-boundary keyword matching.
            (
                "Find files about the commander in Downloads",
                &[FileRead],
                &[MailSend, Trash],
            ),
            // A bare deictic without mail context must not imply attachment save.
            ("Copy that to Documents", &[], &[AttachmentSave, MailSend]),
            // Apostrophe phrasing must still authorize content reads.
            (
                "What's in report.pdf?",
                &[FileRead, FileContentRead],
                &[MailSend, Trash],
            ),
            // Apostrophe negation still blocks sending.
            (
                "Read the email but don't send anything",
                &[MailRead],
                &[MailSend],
            ),
            // System questions authorize read-only system_info, not shell.
            (
                "write a system report about my cpu, memory and disk space",
                &[SystemInfo],
                &[MailSend, Trash],
            ),
            (
                "how much memory and disk space do I have?",
                &[SystemInfo],
                &[MailSend, FileWrite],
            ),
        ];

        for (task, must_grant, must_deny) in cases {
            let auth = TaskAuthorization::from_task(task);
            for cap in *must_grant {
                assert!(
                    cap.granted(&auth),
                    "task {task:?} should GRANT {cap:?} but did not"
                );
            }
            for cap in *must_deny {
                assert!(
                    !cap.granted(&auth),
                    "task {task:?} should DENY {cap:?} but granted it"
                );
            }
        }
    }

    #[test]
    fn task_text_matches_on_word_boundaries() {
        let text = TaskText::new("Find the commander's documents");
        assert!(text.has_phrase(&["documents"]));
        assert!(text.has_phrase(&["find"]));
        // "command" must not match inside "commander".
        assert!(!text.has_phrase(&["command"]));
        // Stem matching still catches morphological variants.
        assert!(text.has_stem(&["command"]));

        let text = TaskText::new("email the report");
        assert!(text.starts_with_word("email"));
        assert!(!text.starts_with_word("report"));

        let text = TaskText::new("resize photo.JPG in ~/Desktop");
        assert!(text.has_file_extension());
        assert!(text.contains_fragment("~/"));

        // Apostrophes are preserved inside words so contraction phrases match.
        let text = TaskText::new("what's in the report?");
        assert!(text.has_phrase(&["what's in"]));
        // A possessive keeps the apostrophe as one token; the bare deictic
        // "it" must not match "it's".
        let text = TaskText::new("send it's contents");
        assert!(!text.has_phrase(&["it"]));
        assert!(text.has_phrase(&["it's contents"]));

        let text = TaskText::new("Φτιάξε φακέλους και βάλε αρχεία");
        assert!(text.has_stem(&["φτιαξ"]));
        assert!(text.has_stem(&["φακελ"]));
        assert!(text.has_stem(&["βαλ"]));
        assert!(text.has_stem(&["αρχε"]));
    }

    #[test]
    fn authorization_audit_snapshot_records_capabilities_and_exposed_tools() {
        let auth =
            TaskAuthorization::from_task("Search the web, then write a report file on my Desktop");
        let snapshot = auth.audit_snapshot(true);
        assert_eq!(snapshot["source"], "current_user_task");
        assert_eq!(snapshot["untrusted_context"], false);
        assert_eq!(snapshot["capabilities"]["web"], true);
        assert_eq!(snapshot["capabilities"]["file_write"], true);
        assert_eq!(snapshot["capabilities"]["mail_send"], false);
        assert_eq!(snapshot["bindings"]["locations"][0], "Desktop");
        let exposed = snapshot["exposed_tools"].as_array().unwrap();
        assert!(exposed.iter().any(|tool| tool == "write_file"));
        assert!(exposed.iter().any(|tool| tool == "openrouter:web_search"));
        assert!(!exposed.iter().any(|tool| tool == "mail_send"));
    }

    #[test]
    fn parsed_intent_separates_capabilities_from_bindings() {
        let intent = ParsedIntent::parse(
            "Email report.pdf to safe@example.com and save the attachment in Documents",
        );
        assert!(intent.capabilities.mail_send);
        assert!(intent.capabilities.mail_attachment_save);
        assert!(intent.capabilities.mail_read);
        assert!(!intent.capabilities.trash);
        assert_eq!(intent.bindings.recipient_count, 1);
        assert_eq!(intent.bindings.attachment_count, 1);
        assert_eq!(
            location_flag_names(intent.bindings.location_flags),
            vec!["Documents"]
        );
    }

    #[test]
    fn derives_high_impact_authorization_from_original_task() {
        let mail = TaskAuthorization::from_task("Send the report to alex@example.com");
        assert!(mail.require_mail_send().is_ok());
        assert!(mail.require_trash().is_err());

        let trash = TaskAuthorization::from_task("Move that folder to Trash");
        assert!(trash.require_trash().is_ok());
        assert!(trash.require_mail_send().is_err());

        let read_only = TaskAuthorization::from_task("Find emails from Alex");
        assert!(read_only.require_mail_send().is_err());
        assert!(read_only.require_trash().is_err());

        let trash_mailbox = TaskAuthorization::from_task("Read my emails in Trash");
        assert!(trash_mailbox.require_trash().is_err());

        let pdf_page = TaskAuthorization::from_task("Remove page 2 from report.pdf");
        assert!(pdf_page.require_trash().is_err());

        let remove_attachment =
            TaskAuthorization::from_task("Remove the attachment from the email");
        assert!(remove_attachment.require_trash().is_err());

        let file_delete = TaskAuthorization::from_task("Delete note.txt");
        assert!(file_delete.require_trash().is_ok());

        let singular_read = TaskAuthorization::from_task("Read my email from Alex");
        assert!(singular_read.require_mail_send().is_err());

        let negated = TaskAuthorization::from_task("Read it but do not send any email");
        assert!(negated.require_mail_send().is_err());

        let imperative = TaskAuthorization::from_task("Please email the report to Alex");
        assert!(imperative.require_mail_send().is_err());

        let addressed = TaskAuthorization::from_task("Send \"hello\" to alex@example.com");
        assert!(addressed.require_mail_send().is_ok());

        let conversational = TaskAuthorization::from_task("Forward it to Alex");
        assert!(conversational.require_mail_send().is_err());
    }

    #[test]
    fn requires_current_task_authorization_for_reads_and_mutations() {
        let question = TaskAuthorization::from_task("What can Finn do?");
        assert!(question.require_tool("mail_search").is_err());
        assert!(question.require_tool("artifact_read").is_err());
        assert!(question.require_tool("write_file").is_err());
        assert!(question.require_tool("create_directory").is_err());
        assert!(question.require_tool("document_create").is_err());

        let mail = TaskAuthorization::from_task("Find emails from Alex");
        assert!(mail.require_tool("mail_search").is_ok());

        let folder = TaskAuthorization::from_task("Does my Desktop folder exist?");
        assert!(folder.require_tool("path_status").is_ok());
        assert!(folder.require_tool("read_file").is_err());
        assert!(folder.require_tool("artifact_read").is_err());

        let document =
            TaskAuthorization::from_task("Read report.docx and create summary.docx in Documents");
        assert!(document.require_tool("artifact_read").is_ok());
        assert!(document.require_tool("document_create").is_ok());

        let script = TaskAuthorization::from_task("Write a zsh script on my Desktop");
        assert!(script.require_tool("write_file").is_ok());
    }

    #[test]
    fn restricts_tools_after_untrusted_external_data() {
        let read_only = TaskAuthorization::from_task("Read my emails").with_untrusted_context(true);
        assert!(read_only.require_tool("mail_read").is_ok());
        assert!(read_only.require_tool("artifact_read").is_err());
        assert!(read_only.require_tool("read_file").is_err());
        assert!(read_only.require_tool("run_shell").is_err());
        assert!(read_only.require_tool("write_file").is_err());
        assert!(read_only.require_tool("mail_send").is_err());
        assert!(read_only.require_tool("mail_save_attachment").is_err());

        let shell = TaskAuthorization::from_task(
            "Read the email and run a shell command to inspect the result",
        )
        .with_untrusted_context(true);
        assert!(shell.require_tool("run_shell").is_err());

        let codex = TaskAuthorization::from_task(
            "Use Codex CLI to build and verify the app in ~/Desktop/test_app",
        )
        .with_untrusted_context(true);
        assert!(codex.require_tool("codex_start").is_ok());
        assert!(codex.require_tool("codex_resume").is_ok());
        assert!(codex.require_tool("run_shell").is_err());

        let attachment = TaskAuthorization::from_task("Save the attachment in Documents")
            .with_untrusted_context(true);
        assert!(attachment.require_tool("mail_save_attachment").is_ok());
        assert!(attachment.require_tool("write_file").is_err());

        let document = TaskAuthorization::from_task("Create summary.docx from the email summary")
            .with_untrusted_context(true);
        assert!(document.require_tool("document_create").is_ok());
        assert!(document.require_tool("artifact_read").is_ok());
        assert!(document.require_tool("run_shell").is_err());
    }

    #[test]
    fn binds_outbound_mail_to_explicit_recipients_and_attachments() {
        let clean = TaskAuthorization::from_task("Send report.pdf to safe@example.com");
        assert!(clean.require_mail_recipient("safe@example.com").is_ok());
        assert!(
            clean
                .require_mail_recipient("attacker@example.com")
                .is_err()
        );
        assert!(
            clean
                .require_outbound_attachments(&[PathBuf::from("/tmp/report.pdf")])
                .is_ok()
        );
        assert!(
            clean
                .require_outbound_attachments(&[PathBuf::from("/tmp/secret.pdf")])
                .is_err()
        );

        let authorized =
            TaskAuthorization::from_task("Read the email and send the summary to safe@example.com")
                .with_untrusted_context(true);
        assert!(
            authorized
                .require_mail_recipient("safe@example.com")
                .is_ok()
        );
        assert!(
            authorized
                .require_mail_recipient("attacker@example.com")
                .is_err()
        );
        assert!(authorized.require_outbound_attachments(&[]).is_ok());
        assert!(
            authorized
                .require_outbound_attachments(&[PathBuf::from("/tmp/secret.pdf")])
                .is_err()
        );

        let with_file =
            TaskAuthorization::from_task("Read the email and send report.pdf to safe@example.com")
                .with_untrusted_context(true);
        let home = PathBuf::from("/Users/tester");
        assert!(
            with_file
                .require_read_path(&home.join("Documents/report.pdf"), &home, true)
                .is_ok()
        );
        assert!(
            with_file
                .require_read_path(&home.join("Documents/secret.pdf"), &home, true)
                .is_err()
        );
        assert!(
            with_file
                .require_outbound_attachments(&[PathBuf::from("/tmp/report.pdf")])
                .is_ok()
        );
        assert!(
            with_file
                .require_outbound_attachments(&[PathBuf::from("/tmp/other.pdf")])
                .is_err()
        );
    }

    #[test]
    fn binds_trash_to_an_explicit_target() {
        let file = TaskAuthorization::from_task("Delete note.txt");
        assert!(file.require_trash_path(Path::new("/tmp/note.txt")).is_ok());
        assert!(
            file.require_trash_path(Path::new("/tmp/other.txt"))
                .is_err()
        );

        let folder = TaskAuthorization::from_task("Move \"Old Reports\" to Trash");
        assert!(
            folder
                .require_trash_path(Path::new("/tmp/Old Reports"))
                .is_ok()
        );
        assert!(
            folder
                .require_trash_path(Path::new("/tmp/Current Reports"))
                .is_err()
        );
    }

    #[test]
    fn requires_explicit_overwrite_authorization() {
        let ordinary = TaskAuthorization::from_task("Create a DOCX report");
        assert!(ordinary.require_overwrite(false).is_ok());
        assert!(ordinary.require_overwrite(true).is_err());

        let explicit =
            TaskAuthorization::from_task("Create a DOCX report and overwrite the existing file");
        assert!(explicit.require_overwrite(true).is_ok());

        let in_place = TaskAuthorization::from_task("Replace Revenue with Profit in report.docx");
        assert!(in_place.require_overwrite(true).is_ok());
    }
}
