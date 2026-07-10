//! Minimal, dependency-free Markdown-to-ANSI renderer for Finn's answers.
//!
//! It is intentionally small: it targets the constructs that appear in typical
//! assistant replies (headings, bold, italics, inline code, fenced code blocks,
//! and bullet/numbered lists) and leaves everything else as plain text. When
//! color is disabled it strips the markers instead of emitting escape codes.

const BOLD: &str = "\x1b[1m";
const ITALIC: &str = "\x1b[3m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const CODE: &str = "\x1b[38;5;215m";
const HEADING: &str = "\x1b[1m\x1b[38;5;39m";
const BULLET: &str = "\x1b[38;5;39m";

/// Renders `input` Markdown into a display string. `color` selects ANSI styling
/// versus plain marker-stripped text.
pub fn render(input: &str, color: bool) -> String {
    let mut out = String::new();
    let mut in_code_block = false;
    let mut fence = "";

    for line in input.lines() {
        let trimmed = line.trim_start();
        // Fenced code blocks: ``` or ~~~. Per CommonMark, a block closes only
        // on the token that opened it; the other token is ordinary content.
        match fence_marker(trimmed) {
            Some(marker) if !in_code_block => {
                in_code_block = true;
                fence = marker;
                continue;
            }
            Some(marker) if marker == fence => {
                in_code_block = false;
                fence = "";
                continue;
            }
            _ => {}
        }
        if in_code_block {
            if color {
                out.push_str(&format!("  {DIM}{line}{RESET}\n"));
            } else {
                out.push_str(&format!("  {line}\n"));
            }
            continue;
        }

        out.push_str(&render_block_line(line, color));
        out.push('\n');
    }
    // Preserve a single trailing newline behavior: trim the last one we added.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Returns the fence token (``` or ~~~) if `trimmed` opens/closes a code fence.
fn fence_marker(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

fn render_block_line(line: &str, color: bool) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    // Headings: one or more leading '#'.
    if let Some(rest) = trimmed.strip_prefix('#') {
        let title = rest.trim_start_matches('#').trim_start();
        return if color {
            format!("{indent}{HEADING}{}{RESET}", render_inline(title, color))
        } else {
            format!("{indent}{}", render_inline(title, color))
        };
    }

    // Bullet lists: '-', '*', or '+' followed by a space.
    for marker in ['-', '*', '+'] {
        if let Some(rest) = trimmed
            .strip_prefix(marker)
            .and_then(|r| r.strip_prefix(' '))
        {
            let dot = if color {
                format!("{BULLET}•{RESET}")
            } else {
                "•".to_owned()
            };
            return format!("{indent}  {dot} {}", render_inline(rest, color));
        }
    }

    // Numbered lists: digits followed by '.' or ')' and a space.
    if let Some(rendered) = render_numbered(trimmed, indent, color) {
        return rendered;
    }

    format!("{indent}{}", render_inline(trimmed, color))
}

fn render_numbered(trimmed: &str, indent: &str, color: bool) -> Option<String> {
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &trimmed[digits.len()..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    let rest = rest.strip_prefix(' ')?;
    let label = if color {
        format!("{BULLET}{digits}.{RESET}")
    } else {
        format!("{digits}.")
    };
    Some(format!("{indent}  {label} {}", render_inline(rest, color)))
}

/// Applies inline styling: bold, italics, and inline code.
fn render_inline(text: &str, color: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        // Inline code: `...`
        if chars[i] == '`'
            && let Some(end) = find_from(&chars, i + 1, '`')
        {
            let inner: String = chars[i + 1..end].iter().collect();
            if color {
                out.push_str(&format!("{CODE}{inner}{RESET}"));
            } else {
                out.push_str(&inner);
            }
            i = end + 1;
            continue;
        }
        // Bold: **...** or __...__
        if i + 1 < chars.len()
            && (chars[i] == '*' || chars[i] == '_')
            && chars[i + 1] == chars[i]
            && let Some(end) = find_double(&chars, i + 2, chars[i])
        {
            let inner: String = chars[i + 2..end].iter().collect();
            let rendered = render_inline(&inner, color);
            if color {
                out.push_str(&format!("{BOLD}{rendered}{RESET}"));
            } else {
                out.push_str(&rendered);
            }
            i = end + 2;
            continue;
        }
        // Italic: *...* or _..._
        if (chars[i] == '*' || chars[i] == '_')
            && let Some(end) = find_from(&chars, i + 1, chars[i])
            && end > i + 1
        {
            let inner: String = chars[i + 1..end].iter().collect();
            if color {
                out.push_str(&format!("{ITALIC}{inner}{RESET}"));
            } else {
                out.push_str(&inner);
            }
            i = end + 1;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn find_from(chars: &[char], start: usize, target: char) -> Option<usize> {
    (start..chars.len()).find(|&index| chars[index] == target)
}

fn find_double(chars: &[char], start: usize, marker: char) -> Option<usize> {
    let mut i = start;
    while i + 1 < chars.len() {
        if chars[i] == marker && chars[i + 1] == marker {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markers_without_color() {
        assert_eq!(render("**bold** text", false), "bold text");
        assert_eq!(render("use `code` here", false), "use code here");
        assert_eq!(render("# Title", false), "Title");
        assert_eq!(render("- item", false), "  • item");
        assert_eq!(render("1. first", false), "  1. first");
        assert_eq!(render("*emphasis*", false), "emphasis");
    }

    #[test]
    fn applies_ansi_with_color() {
        let bold = render("**hi**", true);
        assert!(bold.contains(BOLD));
        assert!(bold.contains("hi"));
        assert!(bold.contains(RESET));

        let code = render("`x`", true);
        assert!(code.contains(CODE));
    }

    #[test]
    fn renders_fenced_code_block_verbatim() {
        let input = "before\n```bash\nls -la **notbold**\n```\nafter";
        let plain = render(input, false);
        assert!(plain.contains("ls -la **notbold**"));
        assert!(!plain.contains("```"));
        assert!(plain.contains("before"));
        assert!(plain.contains("after"));
    }

    #[test]
    fn code_blocks_close_only_on_their_own_fence_token() {
        let input = "```\n~~~ inside\n```\n**after**";
        let plain = render(input, false);
        assert!(plain.contains("~~~ inside"));
        // The block closed on ```, so Markdown after it renders normally.
        assert!(plain.contains("after"));
        assert!(!plain.contains("**after**"));
    }

    #[test]
    fn leaves_unmatched_markers_intact() {
        assert_eq!(render("2 * 3 = 6", false), "2 * 3 = 6");
        assert_eq!(render("a `dangling", false), "a `dangling");
    }

    #[test]
    fn nested_bold_inside_bullets() {
        assert_eq!(render("- **OS:** macOS", false), "  • OS: macOS");
    }
}
