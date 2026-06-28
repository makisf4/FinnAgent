use anyhow::{Result, bail};

const BLOCKED_FRAGMENTS: &[&str] = &[
    "rm -rf /",
    "rm -fr /",
    "rm -rf ~",
    "rm -fr ~",
    "mkfs",
    "diskutil erase",
    "diskutil partition",
    "of=/dev/",
    "chmod -r 777 /",
    "chown -r",
    "shutdown -h",
    "shutdown -r",
    "reboot",
    ":(){:|:&};:",
    ">/dev/",
    "> /dev/",
];

const BLOCKED_COMMANDS: &[&str] = &[
    "rm",
    "rmdir",
    "unlink",
    "srm",
    "dd",
    "mkfs",
    "diskutil",
    "sudo",
    "doas",
    "shutdown",
    "reboot",
    "halt",
    "launchctl",
    "security",
    "osascript",
    "chmod",
    "chown",
    "kill",
    "killall",
    "pkill",
    "curl",
    "wget",
    "nc",
    "ncat",
    "ssh",
    "scp",
    "sftp",
];

const PROTECTED_PATHS: &[&str] = &[
    "~/.ssh",
    "$home/.ssh",
    "~/.gnupg",
    "$home/.gnupg",
    "~/.zshrc",
    "$home/.zshrc",
    "~/library/keychains",
    "$home/library/keychains",
    "/.ssh",
    "/.gnupg",
    "/.aws/credentials",
    "/library/keychains",
    ".zshrc",
];

pub fn validate_shell(command: &str) -> Result<()> {
    let normalized = command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    if normalized.is_empty() {
        bail!("Refusing to run an empty shell command.");
    }
    if BLOCKED_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
    {
        bail!("Catastrophic shell command blocked.");
    }
    if PROTECTED_PATHS
        .iter()
        .any(|protected| normalized.contains(protected))
    {
        bail!("Shell access to a protected credential or configuration path is blocked.");
    }
    for token in shell_command_tokens(&normalized) {
        let command_name = token.rsplit('/').next().unwrap_or(token);
        if BLOCKED_COMMANDS.contains(&command_name) {
            bail!("Shell command '{command_name}' is blocked; use a dedicated audited tool.");
        }
    }
    Ok(())
}

fn shell_command_tokens(command: &str) -> impl Iterator<Item = &str> {
    command
        .split(|character: char| {
            character.is_whitespace()
                || matches!(character, ';' | '|' | '&' | '(' | ')' | '{' | '}')
        })
        .filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_normal_shell_work() {
        assert!(validate_shell("find ~/Desktop -name '*.pdf'").is_ok());
        assert!(validate_shell("mkdir -p ~/Desktop/Makis").is_ok());
    }

    #[test]
    fn blocks_catastrophic_commands() {
        assert!(validate_shell("sudo rm something").is_err());
        assert!(validate_shell("rm -rf /").is_err());
        assert!(validate_shell("diskutil eraseDisk APFS Empty /dev/disk2").is_err());
        assert!(validate_shell("/bin/rm --recursive --force ~/Documents").is_err());
        assert!(validate_shell("curl https://example.com/upload").is_err());
        assert!(validate_shell("cat ~/.ssh/id_ed25519").is_err());
    }
}
