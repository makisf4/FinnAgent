# FinnAgent

Finn is a Rust-based macOS assistant that executes natural-language tasks through either the OpenAI Responses API or OpenRouter Chat Completions API.

Finn does not hand generated commands back to the user. An imperative task is authorization to execute the requested action. Questions remain read-only, deletion moves items to Trash, and catastrophic shell patterns are blocked.

## Capabilities

- Create, inspect, find, read, and write files and folders
- Move exact files and folders to macOS Trash
- Run Bash/Zsh commands and multi-step scripts
- Search and read Apple Mail inbox messages
- Send email with file attachments through Apple Mail when explicitly requested
- Preserve conversation context during the running session
- Record completed task summaries locally
- Show model, reasoning effort, tool activity, API rounds, elapsed time, and real token usage

Apple Mail tools use AppleScript. macOS will request Automation permission the first time Finn accesses Mail.

## Requirements

- macOS
- Rust stable
- An API key for OpenAI or OpenRouter with access to the configured model

## Run

```bash
export FINN_PROVIDER="openai"
export OPENAI_API_KEY="your-key"
cd /Users/makpap/Desktop/Projects/FinnAgent
cargo run --release
```

The release binary is also installed as:

```bash
finn
```

Then speak naturally:

```text
create a folder named Makis on my Desktop
does the folder named Makis exist on my Desktop?
move that folder to Trash
find PDF invoices in my Downloads folder
write a zsh script on my Desktop that reports the ten largest files
find emails from example.com in my inbox
```

Type `/models` during an interactive session to choose from OpenAI and GLM models
or enter an exact custom model ID. The change takes effect immediately and
preserves the current conversation. Selecting a GLM model switches to OpenRouter
and requires `OPENROUTER_API_KEY`. The selection lasts for that Finn session; set
`FINN_PROVIDER` and `FINN_MODEL` to change the startup defaults.

Type `/` and press `Tab` to open slash-command completion with descriptions.
Partial commands such as `/mo` can be completed to `/model` or `/models`.
The model menu discovers current GPT-5 and Z.ai GLM IDs from the provider APIs,
with a built-in fallback list when discovery is unavailable.

Paste or drag a local PNG, JPEG, WEBP, or GIF path into the prompt to send the
image to a vision-capable model. Finn reads and Base64-encodes the local file; it
never asks the model to open the filesystem path directly. OpenRouter image input
automatically routes through `FINN_VISION_MODEL` and then restores the active text
model.

Image understanding is supported. Image generation is not currently implemented,
and Finn will not attempt to synthesize images through shell or filesystem tools.

After every task, Finn displays input/output tokens for that task, cumulative session tokens, cached input, reasoning tokens, tool count, API rounds, and the final provider response ID.

One-shot execution is also supported:

```bash
cargo run --release -- "create a folder named Makis on my Desktop"
```

Check configuration without making an API call:

```bash
cargo run --release -- --check
```

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `FINN_PROVIDER` | `openai` | Provider: `openai` or `openrouter` |
| `OPENAI_API_KEY` | required for OpenAI | OpenAI API authentication |
| `OPENROUTER_API_KEY` | required for OpenRouter | OpenRouter API authentication |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | OpenRouter-compatible API base URL |
| `FINN_MODEL` | provider-specific | `gpt-5.5` for OpenAI; `z-ai/glm-5.2` for OpenRouter |
| `FINN_VISION_MODEL` | `z-ai/glm-5v-turbo` on OpenRouter | Model used automatically for local image input |
| `FINN_REASONING` | `medium` | OpenAI reasoning effort; retained in runtime context for other providers |
| `FINN_HOME` | `~/Library/Application Support/FinnAgent` | Local task log directory |
| `FINN_TASK_LOG` | `on` | Set to `off`, `false`, or `0` to disable local task summaries |
| `FINN_MAIL_SENDER` | `makisf4@gmail.com` | Preferred Apple Mail sender address |

OpenAI setup:

```bash
export FINN_PROVIDER="openai"
export OPENAI_API_KEY="..."
export FINN_MODEL="gpt-5.5"
cargo run --release
```

OpenRouter setup:

```bash
export FINN_PROVIDER=openrouter
export OPENROUTER_API_KEY="..."
export FINN_MODEL="z-ai/glm-5.2"
cargo run --release
```

## Execution Model

1. Finn submits the user's task and typed tool definitions using the selected provider's API format.
2. The model returns function calls.
3. Rust executes each call immediately through an audited handler.
4. Tool results are returned to the model.
5. Finn reports the verified result.

No shell command suggested by the model is executed directly by the API. Every action passes through the Rust tool router and shell safety policy.

OpenAI uses the Responses API. OpenRouter uses `POST /chat/completions` and
OpenAI-compatible function tools. Transient connection failures, HTTP 429
responses, and server errors are retried with bounded backoff. Requests have
connect and overall timeouts.

## Safety Boundary

Finn is intentionally autonomous. It does not ask for a second confirmation after an explicit task.

The shell handler blocks destructive utilities, privilege escalation, network
utilities, credential paths, disk operations, process-control commands, and
similar high-risk patterns. File deletion uses Trash. Rust independently checks
that the original user task authorized email sending or moving an item to Trash;
model instructions alone cannot grant those capabilities.

Reads, writes, attachments, and shell access to `.ssh`, `.gnupg`, AWS
credentials, shell startup files, and macOS Keychains are blocked. Tool output is
treated as untrusted content to reduce prompt-injection risk. Task logs are
created with user-only `0600` permissions.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
