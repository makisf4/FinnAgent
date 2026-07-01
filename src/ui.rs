use std::env;
use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{Duration, sleep};

use crate::agent::{TaskResult, Usage};
use crate::config::{Config, ModelOption, Provider};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const BLUE: &str = "\x1b[38;5;39m";
const YELLOW: &str = "\x1b[38;5;179m";
const RED: &str = "\x1b[38;5;203m";
const GREY: &str = "\x1b[38;5;245m";

const RULE_WIDTH: usize = 64;

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Animated single-line activity indicator shown while Finn waits on the model
/// or runs a tool. Renders to stdout only when stdout is a terminal; otherwise
/// it is inert so piped/redirected output stays clean.
pub struct Spinner {
    label: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    suppressed: Arc<AtomicBool>,
    /// Set by the animation task once it has cleared its line in response to
    /// `suppressed`, and cleared again on `resume`. A streaming sink waits for
    /// this before writing so the animation cannot overwrite the answer's first
    /// bytes (the classic "the first characters are cut off" race).
    quiesced: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}

impl Spinner {
    /// Starts a spinner with an initial `label`. On a non-terminal stdout the
    /// spinner does nothing, so callers can use it unconditionally.
    pub fn start(label: impl Into<String>) -> Self {
        let label = Arc::new(Mutex::new(label.into()));
        let stop = Arc::new(AtomicBool::new(false));
        let suppressed = Arc::new(AtomicBool::new(false));
        let quiesced = Arc::new(AtomicBool::new(false));
        if !io::stdout().is_terminal() || !color_enabled() {
            // No animation runs, so a sink never has to wait for the line to be
            // clear: mark it quiesced up front.
            quiesced.store(true, Ordering::Relaxed);
            return Self {
                label,
                stop,
                suppressed,
                quiesced,
                task: None,
            };
        }
        let task_label = Arc::clone(&label);
        let task_stop = Arc::clone(&stop);
        let task_suppressed = Arc::clone(&suppressed);
        let task_quiesced = Arc::clone(&quiesced);
        let task = tokio::spawn(async move {
            let started = Instant::now();
            let mut frame = 0_usize;
            let mut cleared = false;
            while !task_stop.load(Ordering::Relaxed) {
                // When suppressed (e.g. the answer is streaming), clear the line
                // once and stop drawing, but keep the task alive so stop() joins.
                if task_suppressed.load(Ordering::Relaxed) {
                    if !cleared {
                        let mut out = io::stdout().lock();
                        let _ = write!(out, "\r\x1b[2K");
                        let _ = out.flush();
                        cleared = true;
                    }
                    // Announce that the line is clear and we are done drawing so
                    // a waiting streaming sink can safely take over the line.
                    task_quiesced.store(true, Ordering::Release);
                    sleep(Duration::from_millis(50)).await;
                    continue;
                }
                cleared = false;
                task_quiesced.store(false, Ordering::Relaxed);
                let text = task_label.lock().await.clone();
                let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
                let elapsed = started.elapsed().as_secs_f64();
                {
                    let mut out = io::stdout().lock();
                    let _ = write!(
                        out,
                        "\r\x1b[2K{BLUE}{glyph}{RESET} {text} {DIM}{elapsed:.1}s{RESET}"
                    );
                    let _ = out.flush();
                }
                frame += 1;
                sleep(Duration::from_millis(90)).await;
            }
        });
        Self {
            label,
            stop,
            suppressed,
            quiesced,
            task: Some(task),
        }
    }

    /// Returns the flag the animation checks to suppress drawing. Flipping it to
    /// `true` lets a synchronous streaming sink take over the line safely. After
    /// setting it, callers must call [`wait_until_quiet`] before writing so the
    /// animation cannot overwrite the first bytes they emit.
    pub fn suppressor(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.suppressed)
    }

    /// The acknowledgement flag the animation sets once it has cleared its line
    /// and stopped drawing in response to suppression. See [`wait_until_quiet`].
    pub fn quiesced_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.quiesced)
    }

    /// Re-enables animation after a suppressed stretch.
    pub fn resume(&self) {
        self.quiesced.store(false, Ordering::Relaxed);
        self.suppressed.store(false, Ordering::Relaxed);
    }

    /// Replaces the activity label shown next to the spinner.
    pub async fn set_label(&self, label: impl Into<String>) {
        *self.label.lock().await = label.into();
    }

    /// Clears the current spinner line so the caller can print a durable line
    /// without the animation overwriting it. The animation, if running, redraws
    /// on its next frame below the printed output.
    pub async fn pause_line(&self) {
        if self.task.is_some() {
            let mut out = io::stdout().lock();
            let _ = write!(out, "\r\x1b[2K");
            let _ = out.flush();
        }
    }

    /// Suppresses animation while an interactive terminal prompt owns stdout.
    /// Unlike [`pause_line`], this waits for the animation task to acknowledge
    /// suppression so it cannot immediately redraw over the prompt.
    pub async fn pause_for_prompt(&self) {
        self.suppressed.store(true, Ordering::Release);
        if self.task.is_none() {
            return;
        }
        for _ in 0..40 {
            if self.quiesced.load(Ordering::Acquire) {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }

    /// Stops the animation and clears the spinner line.
    pub async fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            let _ = task.await;
            let mut out = io::stdout().lock();
            let _ = write!(out, "\r\x1b[2K");
            let _ = out.flush();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        // If the owning task is cancelled (e.g. Ctrl-C) without calling stop(),
        // signal the animation to end and clear the line so the terminal is not
        // left with a dangling spinner or a background task still drawing.
        self.stop.store(true, Ordering::Relaxed);
        if self.task.is_some() {
            let mut out = io::stdout().lock();
            let _ = write!(out, "\r\x1b[2K");
            let _ = out.flush();
        }
    }
}

/// Blocks until the spinner animation has acknowledged suppression by clearing
/// its line, so a synchronous streaming sink can write without the animation
/// overwriting the first bytes of the answer. `quiesced` is the flag from
/// [`Spinner::quiesced_flag`]. The wait is bounded so a missing or already
/// stopped animation can never hang the caller.
pub fn wait_until_quiet(quiesced: &AtomicBool) {
    // The animation polls suppression every ~50ms; a handful of short spins
    // comfortably covers one poll interval without a hard dependency on timing.
    for _ in 0..400 {
        if quiesced.load(Ordering::Acquire) {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub struct CommandSpec {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "/commands",
        description: "Show the command list",
    },
    CommandSpec {
        name: "/help",
        description: "Show help",
    },
    CommandSpec {
        name: "/model",
        description: "Select the active model",
    },
    CommandSpec {
        name: "/models",
        description: "Select the active model",
    },
    CommandSpec {
        name: "/clear",
        description: "Start a fresh conversation",
    },
    CommandSpec {
        name: "/exit",
        description: "Leave Finn",
    },
    CommandSpec {
        name: "/quit",
        description: "Leave Finn",
    },
];

pub fn render_startup(config: &Config, tool_count: usize) {
    // Each content line is a (plain, colored) pair. The plain text is measured
    // to size the box so nothing overflows the border; the colored text is what
    // gets printed.
    let mut lines: Vec<(String, String)> = vec![
        (
            format!("Finn v{VERSION}  ·  natural-language macOS assistant"),
            format!(
                "{} {}",
                style("Finn", &format!("{BOLD}{BLUE}")),
                style(
                    &format!("v{VERSION}  ·  natural-language macOS assistant"),
                    DIM
                )
            ),
        ),
        (
            format!(
                "model {}  reasoning {}  tools {}  api {}",
                config.model,
                config.reasoning_effort,
                tool_count,
                config.provider.api_label()
            ),
            format!(
                "{}  {}  {}  {}",
                field("model", &config.model),
                field("reasoning", &config.reasoning_effort),
                field("tools", &tool_count.to_string()),
                field("api", config.provider.api_label())
            ),
        ),
    ];
    if let Some(vision_model) = &config.vision_model {
        lines.push((
            format!("vision route {vision_model}"),
            field("vision route", vision_model),
        ));
    }

    // Inner width = widest plain line, with two spaces of padding on each side.
    let inner = lines
        .iter()
        .map(|(plain, _)| plain.chars().count())
        .max()
        .unwrap_or(0)
        + 4;

    println!("{}", style(&format!("╭{}╮", "─".repeat(inner)), BLUE));
    for (plain, colored) in &lines {
        let pad = inner.saturating_sub(plain.chars().count() + 2);
        println!(
            "{}  {}{}{}",
            style("│", BLUE),
            colored,
            " ".repeat(pad),
            style("│", BLUE)
        );
    }
    println!("{}", style(&format!("╰{}╯", "─".repeat(inner)), BLUE));
    println!(
        "{}",
        style(
            "Tell Finn what to do. Questions stay read-only; tasks run immediately.",
            DIM
        )
    );
    println!(
        "{}",
        style(
            "Type / then Tab for commands  ·  /clear to reset  ·  end a line with \\ for multi-line.",
            DIM
        )
    );
    println!(
        "{}",
        style(
            "Ctrl-C cancels a running task  ·  Ctrl-C at the prompt exits.",
            DIM
        )
    );
    println!();
}

pub fn render_commands() {
    println!("{}", style("Commands", &format!("{BOLD}{BLUE}")));
    for command in COMMANDS {
        println!(
            "  {}  {}",
            style(&format!("{:<10}", command.name), CYAN),
            style(command.description, DIM)
        );
    }
    println!();
    println!(
        "{}",
        style(
            "Everything else is a natural-language task. Paste or drag an image path to send it.",
            DIM
        )
    );
    println!();
}

pub fn render_models(active_provider: Provider, active_model: &str, models: &[ModelOption]) {
    println!("{}", style("Available models", &format!("{BOLD}{BLUE}")));
    for (index, model) in models.iter().enumerate() {
        let is_active = model.provider == active_provider && model.id == active_model;
        let marker = if is_active {
            style("●", GREEN)
        } else {
            style("·", DIM)
        };
        let id = if is_active {
            style(&model.id, BOLD)
        } else {
            model.id.clone()
        };
        println!(
            "  {} {:>2}. {}  {}",
            marker,
            index + 1,
            id,
            style(&format!("[{}]", model.provider), DIM)
        );
    }
    println!();
    println!(
        "{}",
        style(
            "Enter a number or an exact custom model ID. Press Enter to cancel.",
            DIM
        )
    );
}

pub fn resolve_model_selection(
    input: &str,
    models: &[ModelOption],
    custom_provider: Provider,
) -> Result<Option<(Provider, String)>, String> {
    let selection = input.trim();
    if selection.is_empty() {
        return Ok(None);
    }
    if let Ok(index) = selection.parse::<usize>() {
        return models
            .get(index.wrapping_sub(1))
            .map(|model| Some((model.provider, model.id.clone())))
            .ok_or_else(|| format!("Model selection must be between 1 and {}.", models.len()));
    }
    Ok(Some((custom_provider, selection.to_owned())))
}

pub fn clear_screen() {
    print!("\x1b[2J\x1b[H");
    let _ = io::stdout().flush();
}

/// Builds a readline prompt string. ANSI codes are wrapped in the `\x01`/`\x02`
/// markers rustyline uses to exclude non-printing bytes from width counting.
pub fn prompt(label: &str) -> String {
    if color_enabled() {
        format!("\x01{CYAN}\x02{label} ›\x01{RESET}\x02 ")
    } else {
        format!("{label} > ")
    }
}

pub fn render_tool_call(index: u64, name: &str, status: &str) {
    let (glyph, glyph_color) = match status {
        "complete" => ("✔", GREEN),
        "denied" => ("⊘", YELLOW),
        _ => ("✗", RED),
    };
    println!(
        "  {} {} {}{}",
        style(glyph, glyph_color),
        style(&tool_label(name), BOLD),
        style(&format!("#{index}"), DIM),
        if status == "complete" {
            String::new()
        } else {
            style(&format!("  {status}"), glyph_color)
        }
    );
}

/// The header line printed above Finn's answer, e.g. `● Finn`.
pub fn answer_header() -> String {
    format!(
        "{} {}",
        style("●", GREEN),
        style("Finn", &format!("{BOLD}{GREEN}"))
    )
}

pub fn render_task_result(result: &TaskResult, reasoning: &str) {
    // When the answer was streamed live, its body and header are already on
    // screen; only add spacing and the summary. Otherwise print the header and
    // the markdown-rendered answer here.
    if result.answer_streamed {
        println!();
    } else {
        println!();
        println!("{}", answer_header());
        println!(
            "{}",
            crate::markdown::render(&result.answer, color_enabled())
        );
        println!();
    }

    let rule = "─".repeat(RULE_WIDTH);
    println!("{}", style(&rule, GREY));
    println!(
        "  {}   {}   {}   {}",
        field("turn", &result.turn.to_string()),
        field("model", &result.model),
        field("reasoning", reasoning),
        field("tools", &result.tool_calls.to_string())
    );
    println!(
        "  {}   {}   {}",
        field("tokens", &format_usage(result.task_usage)),
        field("session", &format_number(result.session_usage.total_tokens)),
        field("rounds", &result.api_rounds.to_string())
    );
    let mut extras = vec![field("elapsed", &format_duration(result.elapsed_ms))];
    if result.task_usage.cached_input_tokens > 0 {
        extras.push(field(
            "cached",
            &format_number(result.task_usage.cached_input_tokens),
        ));
    }
    if result.task_usage.reasoning_tokens > 0 {
        extras.push(field(
            "reasoning tokens",
            &format_number(result.task_usage.reasoning_tokens),
        ));
    }
    if !result.response_id.is_empty() {
        extras.push(field("response", &short_id(&result.response_id)));
    }
    println!("  {}", extras.join("   "));
    println!("{}", style(&rule, GREY));
    println!();
}

fn format_usage(usage: Usage) -> String {
    format!(
        "{} in + {} out = {}",
        format_number(usage.input_tokens),
        format_number(usage.output_tokens),
        format_number(usage.total_tokens)
    )
}

fn field(name: &str, value: &str) -> String {
    format!("{} {}", style(name, GREY), style(value, BOLD))
}

pub fn tool_label(name: &str) -> String {
    name.replace('_', " ")
}

fn short_id(value: &str) -> String {
    const MAX: usize = 28;
    if value.len() <= MAX {
        value.to_owned()
    } else {
        format!("{}...", &value[..MAX])
    }
}

fn format_number(value: u64) -> String {
    let digits = value.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(',');
        }
        result.push(character);
    }
    result
}

fn format_duration(milliseconds: u128) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds}ms")
    } else {
        format!("{:.2}s", milliseconds as f64 / 1_000.0)
    }
}

fn style(value: &str, code: &str) -> String {
    if color_enabled() {
        format!("{code}{value}{RESET}")
    } else {
        value.to_owned()
    }
}

fn color_enabled() -> bool {
    env::var_os("NO_COLOR").is_none() && env::var("TERM").is_ok_and(|term| term != "dumb")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spinner_lifecycle_is_safe_without_a_terminal() {
        // In the test harness stdout is not a terminal, so the spinner is
        // inert. Starting, relabeling, pausing, and stopping must not panic and
        // must leave no background task running.
        let spinner = Spinner::start("Thinking");
        spinner.set_label("Running path status").await;
        spinner.pause_line().await;
        spinner.pause_for_prompt().await;
        assert!(spinner.suppressed.load(Ordering::Acquire));
        spinner.resume();
        assert!(!spinner.suppressed.load(Ordering::Acquire));
        spinner.stop().await;
    }

    #[tokio::test]
    async fn inert_spinner_reports_quiesced_so_sinks_never_block() {
        // Without a terminal there is no animation to overwrite streamed output,
        // so a waiting sink must observe the line as already quiet and not spin
        // for the full timeout.
        let spinner = Spinner::start("Thinking");
        assert!(spinner.quiesced_flag().load(Ordering::Acquire));
        let started = Instant::now();
        wait_until_quiet(&spinner.quiesced_flag());
        assert!(started.elapsed() < Duration::from_millis(50));
        spinner.stop().await;
    }

    #[test]
    fn wait_until_quiet_returns_once_the_flag_is_set() {
        let flag = AtomicBool::new(false);
        // A brief spin then set from another thread mirrors the animation task
        // acknowledging suppression while the sink waits.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(5));
                flag.store(true, Ordering::Release);
            });
            wait_until_quiet(&flag);
        });
        assert!(flag.load(Ordering::Acquire));
    }

    #[test]
    fn formats_token_counts() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1_234), "1,234");
        assert_eq!(format_number(1_234_567), "1,234,567");
    }

    #[test]
    fn formats_elapsed_duration() {
        assert_eq!(format_duration(845), "845ms");
        assert_eq!(format_duration(1_234), "1.23s");
    }

    #[test]
    fn formats_usage_summary() {
        let usage = Usage {
            input_tokens: 1_200,
            output_tokens: 300,
            total_tokens: 1_500,
            ..Usage::default()
        };
        assert_eq!(format_usage(usage), "1,200 in + 300 out = 1,500");
    }

    #[test]
    fn resolves_menu_and_custom_model_selections() {
        let models = [
            ModelOption {
                provider: Provider::OpenAi,
                id: "model-a".to_owned(),
            },
            ModelOption {
                provider: Provider::OpenRouter,
                id: "model-b".to_owned(),
            },
        ];
        assert_eq!(
            resolve_model_selection("2", &models, Provider::OpenAi).unwrap(),
            Some((Provider::OpenRouter, "model-b".to_owned()))
        );
        assert_eq!(
            resolve_model_selection("provider/custom", &models, Provider::OpenAi).unwrap(),
            Some((Provider::OpenAi, "provider/custom".to_owned()))
        );
        assert_eq!(
            resolve_model_selection("", &models, Provider::OpenAi).unwrap(),
            None
        );
        assert!(resolve_model_selection("3", &models, Provider::OpenAi).is_err());
    }
}
