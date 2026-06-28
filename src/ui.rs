use std::env;
use std::io::{self, Write};

use crate::agent::{TaskResult, Usage};
use crate::config::{Config, ModelOption, Provider};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

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
        name: "/exit",
        description: "Leave Finn",
    },
    CommandSpec {
        name: "/quit",
        description: "Leave Finn",
    },
];

pub fn render_startup(config: &Config, tool_count: usize) {
    let line = "-".repeat(72);
    println!("{}", style(&line, DIM));
    println!(
        "{} {}",
        style("FinnAgent", BOLD),
        style(&format!("v{VERSION}"), DIM)
    );
    println!(
        "{}  {}  {}  {}",
        field("model", &config.model),
        field("reasoning", &config.reasoning_effort),
        field("tools", &tool_count.to_string()),
        field("mode", "direct")
    );
    println!(
        "{}  {}",
        field("api", config.provider.api_label()),
        field("session tokens", "0")
    );
    if let Some(vision_model) = &config.vision_model {
        println!("{}", field("future vision model", vision_model));
    }
    println!("{}", style(&line, DIM));
    println!("Tell Finn what to do. Questions remain read-only; tasks execute immediately.");
    println!("Type / then Tab for commands, or /commands for the full list.");
    println!("Use Up/Down for history, Left/Right to edit, Ctrl-C to exit.");
    println!();
}

pub fn render_commands() {
    println!("Available commands:");
    for command in COMMANDS {
        println!("  {:<10} {}", command.name, command.description);
    }
    println!();
    println!("Everything else is treated as a natural-language task.");
    println!("Paste or drag an image path to send the image to the active model.");
    println!();
}

pub fn render_models(active_provider: Provider, active_model: &str, models: &[ModelOption]) {
    println!("Available models:");
    for (index, model) in models.iter().enumerate() {
        let active = if model.provider == active_provider && model.id == active_model {
            " (active)"
        } else {
            ""
        };
        println!(
            "  {}. {}  [{}]{}",
            index + 1,
            model.id,
            model.provider,
            active
        );
    }
    println!();
    println!("Enter a number or an exact custom model ID. Press Enter to cancel.");
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

pub fn render_tool_call(index: u64, name: &str) {
    println!(
        "{} {}",
        style(&format!("[tool {index}]"), CYAN),
        tool_label(name)
    );
}

pub fn render_task_result(result: &TaskResult, model: &str, reasoning: &str) {
    println!();
    println!("{} {}", style("Finn", GREEN), result.answer);
    println!();
    let line = "-".repeat(72);
    println!("{}", style(&line, DIM));
    println!(
        "{}  {}  {}  {}",
        field("turn", &result.turn.to_string()),
        field("model", model),
        field("reasoning", reasoning),
        field("tools", &result.tool_calls.to_string())
    );
    println!(
        "{}  {}  {}  {}",
        field("task tokens", &format_usage(result.task_usage)),
        field("session", &format_number(result.session_usage.total_tokens)),
        field("API rounds", &result.api_rounds.to_string()),
        field("elapsed", &format_duration(result.elapsed_ms))
    );
    if result.task_usage.cached_input_tokens > 0 || result.task_usage.reasoning_tokens > 0 {
        println!(
            "{}  {}",
            field(
                "cached input",
                &format_number(result.task_usage.cached_input_tokens)
            ),
            field(
                "reasoning tokens",
                &format_number(result.task_usage.reasoning_tokens)
            )
        );
    }
    if !result.response_id.is_empty() {
        println!("{}", field("response", &short_id(&result.response_id)));
    }
    println!("{}", style(&line, DIM));
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
    format!("{} {}", style(name, DIM), value)
}

fn tool_label(name: &str) -> String {
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
