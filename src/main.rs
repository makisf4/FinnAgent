mod agent;
mod catalog;
mod config;
mod input;
mod prompt;
mod provider;
mod safety;
mod tools;
mod ui;

use std::env;

use agent::Agent;
use anyhow::Result;
use config::Config;
use prompt::SlashHelper;
use rustyline::config::BellStyle;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{CompletionType, Config as ReadlineConfig, Editor};
use tools::ToolContext;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--check") {
        let provider = env::var("FINN_PROVIDER")
            .unwrap_or_else(|_| "openai".to_owned())
            .parse::<config::Provider>()?;
        let model = env::var("FINN_MODEL").unwrap_or_else(|_| provider.default_model().to_owned());
        let reasoning = env::var("FINN_REASONING").unwrap_or_else(|_| "medium".to_owned());
        let key_name = match provider {
            config::Provider::OpenAi => "OPENAI_API_KEY",
            config::Provider::OpenRouter => "OPENROUTER_API_KEY",
        };
        let key_status = if env::var(key_name).is_ok_and(|key| !key.trim().is_empty()) {
            "set"
        } else {
            "missing"
        };
        println!("Finn check:");
        println!("provider: {provider}");
        println!("model: {model}");
        println!("reasoning: {reasoning}");
        println!("{key_name}: {key_status}");
        return Ok(());
    }

    let mut config = Config::load()?;
    tokio::fs::create_dir_all(&config.data_dir).await?;

    let context = ToolContext::new(config.home.clone(), config.data_dir.clone());
    let mut agent = Agent::new(config.clone(), context)?;

    if !args.is_empty() {
        let result = agent.run_task(&args.join(" ")).await?;
        ui::render_task_result(&result, &config.model, &config.reasoning_effort);
        return Ok(());
    }

    ui::clear_screen();
    ui::render_startup(&config, tools::definitions().len());
    let readline_config = ReadlineConfig::builder()
        .completion_type(CompletionType::List)
        .completion_show_all_if_ambiguous(true)
        .bell_style(BellStyle::None)
        .build();
    let mut editor = Editor::<SlashHelper, DefaultHistory>::with_config(readline_config)?;
    editor.set_helper(Some(SlashHelper));

    loop {
        let line = match editor.readline("finn > ") {
            Ok(line) => line,
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => break,
            Err(error) => return Err(error.into()),
        };
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        if let Some(path) = input::pasted_image_path(task).await {
            let previous_config = config.clone();
            let routed_to_vision = config
                .vision_model
                .clone()
                .filter(|vision_model| vision_model != &config.model);
            if let Some(vision_model) = &routed_to_vision {
                match config.switched(config.provider, vision_model) {
                    Ok(vision_config) => {
                        println!(
                            "Image route: {} [{}]",
                            vision_config.model, vision_config.provider
                        );
                        agent.switch_model(vision_config.clone());
                        config = vision_config;
                    }
                    Err(error) => {
                        eprintln!("Cannot select image model {vision_model}: {error:#}");
                        continue;
                    }
                }
            }
            let data_url = match input::image_data_url(&path).await {
                Ok(data_url) => data_url,
                Err(error) => {
                    eprintln!("Cannot load image: {error:#}");
                    continue;
                }
            };
            let prompt = "Analyze this image. Describe what you see and respond helpfully.";
            let log_task = format!("[image: {}]", path.display());
            match agent.run_image_task(prompt, &data_url, &log_task).await {
                Ok(result) => {
                    ui::render_task_result(&result, &config.model, &config.reasoning_effort)
                }
                Err(error) => eprintln!("Image task failed: {error:#}"),
            }
            if routed_to_vision.is_some() {
                agent.switch_model(previous_config.clone());
                config = previous_config;
                println!(
                    "Active model restored: {} [{}]",
                    config.model, config.provider
                );
            }
            continue;
        }
        if task.starts_with('/') {
            match task.to_ascii_lowercase().as_str() {
                "/commands" | "/help" => ui::render_commands(),
                "/model" | "/models" => {
                    println!("Loading available models...");
                    let catalog = catalog::discover().await;
                    for warning in &catalog.warnings {
                        eprintln!("Model catalog warning: {warning}");
                    }
                    ui::render_models(config.provider, &config.model, &catalog.models);
                    let selection = match editor.readline("model > ") {
                        Ok(selection) => selection,
                        Err(ReadlineError::Interrupted | ReadlineError::Eof) => continue,
                        Err(error) => return Err(error.into()),
                    };
                    match ui::resolve_model_selection(&selection, &catalog.models, config.provider)
                    {
                        Ok(Some((provider, model))) => match config.switched(provider, &model) {
                            Ok(selected_config) => {
                                agent.switch_model(selected_config.clone());
                                config = selected_config;
                                println!("Active model: {} [{}]", config.model, config.provider);
                            }
                            Err(error) => eprintln!("Cannot select {model}: {error:#}"),
                        },
                        Ok(None) => {}
                        Err(error) => eprintln!("{error}"),
                    }
                }
                "/exit" | "/quit" => break,
                _ => eprintln!("Unknown command: {task}. Type /commands for a list."),
            }
            continue;
        }
        if matches!(task.to_ascii_lowercase().as_str(), "exit" | "quit") {
            break;
        }
        editor.add_history_entry(task)?;

        match agent.run_task(task).await {
            Ok(result) => ui::render_task_result(&result, &config.model, &config.reasoning_effort),
            Err(error) => eprintln!("Task failed: {error:#}"),
        }
    }

    Ok(())
}
