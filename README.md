# FinnAgent 0.4.0

Finn is a Rust-based macOS assistant that executes natural-language tasks through OpenRouter.

Finn does not hand generated commands back to the user. An imperative task is authorization to execute the requested action. Questions remain read-only, deletion moves explicitly named items to Trash, and general shell execution is unavailable.

## Capabilities

- Create, inspect, find, read, and write files and folders
- Inspect and extract text/content from TXT, DOCX, PDF, and XLSX files
- Create TXT and basic styled DOCX documents and replace their text
- Create or edit XLSX cells with text, numbers, booleans, and formulas
- Replace PDF text and remove or rotate selected PDF pages
- Inspect, convert, resize, crop, rotate, flip, and grayscale PNG, JPEG, GIF, WEBP, and TIFF images
- Move exact files and folders to macOS Trash
- Report read-only system information: OS, CPU, memory, and root-disk usage
- Search for and download public HTTPS images or files to an exact local path
- Optionally run explicitly requested Bash/Zsh commands in a secret-free subprocess
- Search and read Apple Mail messages in Inbox, Trash, Junk, Sent, and Drafts
- Find recent attachments by file type without scanning the whole mailbox, then list and save them
- Send email with file attachments through Apple Mail when explicitly requested
- Preserve conversation context during the running session
- Optionally record completed task summaries locally
- Include task-level and per-tool authorization summaries in opted-in local task logs
- Show a live activity indicator while thinking or running tools, then a per-task summary of model, reasoning effort, tool activity, API rounds, elapsed time, and real token usage
- Stream the answer live as it is generated, and render Markdown (bold, code, lists) in the final reply
- Work in batches: if a long task reaches the step budget without finishing, an interactive session asks whether to keep going instead of failing outright
- Generate images with selected OpenRouter image models and save them under `~/Pictures/Finn`

Apple Mail tools use AppleScript. macOS will request Automation permission the first time Finn accesses Mail.

## Requirements

- macOS
- Rust stable
- An OpenRouter API key with access to the configured model

## Run

```bash
export OPENROUTER_API_KEY="your-key"
cd FinnAgent
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
move ~/Desktop/Makis to Trash
find PDF invoices in my Downloads folder
write a zsh script on my Desktop that reports the ten largest files
find emails from example.com in my inbox
save the invoice attached to Alex's email in ~/Documents/Invoices
create a DOCX report in ~/Documents and verify its text
update Summary!B4 in ~/Documents/budget.xlsx to the formula =B2-B3
rotate page 2 of ~/Downloads/scan.pdf by 90 degrees
resize ~/Desktop/photo.jpg to 1200 by 800 pixels and save it as PNG
download a photo of Larry Bird to my Desktop
```

Type `/models` during an interactive session to choose a curated OpenRouter
model or enter an exact custom model ID. The list includes OpenAI, Anthropic,
Google, xAI, DeepSeek, Qwen, Kimi, Mistral, MiniMax, Meta, Z.ai, and image
generation models. The change takes effect immediately. Prior text turns are
replayed onto the new model, but tool results and image inputs are not carried
over. The selection lasts for that Finn session; set `FINN_MODEL` to change the
startup default.

Type `/` and press `Tab` to open slash-command completion with descriptions.
Partial commands such as `/mo` can be completed to `/model` or `/models`.
The model menu validates the curated list against OpenRouter's live model
metadata and falls back to a built-in list when discovery is unavailable.

Type `/clear` (or `/reset`, `/new`) to discard the current conversation and start
fresh on the same model. End a line with a backslash `\` to continue onto the
next line, and unclosed triple-backtick code fences keep reading until closed, so
multi-line tasks and pasted code blocks work at the prompt. Press `Ctrl-C` while a
task is running to cancel it and return to the prompt with the conversation
unchanged; `Ctrl-C` at an empty prompt exits Finn.

Paste or drag a local PNG, JPEG, WEBP, or GIF path into the prompt to send the
image to a vision-capable model. Finn reads and Base64-encodes the local file; it
never asks the model to open the filesystem path directly.

OpenRouter uses a unified hybrid orchestrator. Text turns route to `FINN_MODEL`
(`z-ai/glm-5.2` by default); image input and explicit visual-verification work
route to `FINN_VISION_MODEL` (`z-ai/glm-5v-turbo` by default). The visual route
remains active through its tool loop. A later nonvisual user turn returns to the
text model after recursively replacing image URLs, Base64 image data, multipart
image objects, and visual data in tool-call arguments with a typed sanitation
marker. OpenRouter `reasoning_details` are retained exactly for subsequent turns
on the model that produced them and are not forwarded across models.

Image understanding is supported through assistant models. When an image
generation model is active, natural-language input is sent to OpenRouter's
`/images` API and the generated file is saved under `~/Pictures/Finn`.

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
| `OPENROUTER_API_KEY` | required for OpenRouter | OpenRouter API authentication |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | OpenRouter-compatible API base URL |
| `FINN_MODEL` | `z-ai/glm-5.2` | Startup OpenRouter model |
| `FINN_VISION_MODEL` | `z-ai/glm-5v-turbo` | Model used automatically for local image input |
| `FINN_REASONING` | `medium` | OpenRouter reasoning configuration |
| `FINN_HOME` | `~/Library/Application Support/FinnAgent` | Local task log directory |
| `FINN_TASK_LOG` | `off` | Set to `1`, `true`, `yes`, or `on` to retain local task summaries |
| `FINN_MAIL_SENDER` | first enabled account | Preferred Apple Mail sender address; defaults to the first enabled Apple Mail account |

OpenRouter setup:

```bash
export OPENROUTER_API_KEY="..."
export FINN_MODEL="z-ai/glm-5.2"
cargo run --release
```

## Execution Model

1. Finn submits the user's task and typed tool definitions through OpenRouter.
2. The model returns function calls.
3. Rust executes each call immediately through an audited handler.
4. Tool results are returned to the model.
5. Finn reports the verified result.

No shell command suggested by the model is executed directly by the API. Every
action passes through a dedicated Rust tool handler. General shell execution is
unavailable; local actions use bounded filesystem, artifact, Mail, download, and
system-information tools.

Codex CLI delegation uses separate `codex_start` and `codex_resume` tools. When
the user explicitly asks Finn to use or supervise Codex,
Finn starts `codex exec --json` in a workspace below the user's home, reads the
JSONL transcript and session ID, and may make up to eight focused resume calls.
Codex runs with its `workspace-write` sandbox, provider API keys are removed from
its environment, and its output is always treated as untrusted data.

Explicit requests to search, browse, research, or fetch online content expose
OpenRouter's model-independent `web_search` and `web_fetch` server tools to the
currently selected assistant model. Search is capped at five results per call
and ten per request; fetch is capped at five uses and 12,000 content tokens.
OpenRouter executes these tools outside the Mac, and Finn marks their use as
untrusted external context before any local tool call is considered. Web search
may incur additional OpenRouter charges.

For tasks that combine research with local work, Finn holds the research answer,
then starts a local-only continuation round with the authorized filesystem or
artifact tools. This prevents a model from ending the task after research or
mistaking OpenRouter's internal server-tool events for client-side function
calls.

When the user asks Finn to download an online image or file without supplying a
direct URL, Finn can search for a suitable source and pass its direct public
HTTPS URL to the local `download_url` tool. Downloads are capped at 25 MiB,
follow at most five redirects, reject credentials and private or local network
addresses, and are written through a temporary file before being moved into
place. Existing destinations are never replaced unless the original request
explicitly authorizes overwriting and the interactive confirmation succeeds.

OpenRouter uses `POST /chat/completions` with OpenAI-compatible function tools,
and `POST /images` for image-generation models. Transient connection failures, HTTP 429
responses, and server errors are retried with bounded backoff. Requests have
connect and overall timeouts. OpenRouter requests also include trusted host
context collected from macOS (`sw_vers`, `uname -m`, and `$SHELL`) so generated
commands use the correct local platform syntax. Concurrent API turns are
serialized, and stale responses are discarded if history changes in flight.

## Safety Boundary

Finn is intentionally autonomous for low-impact work and does not ask for a
second confirmation on ordinary reads or fresh writes. High-impact,
hard-to-reverse actions are the exception: in an interactive session Finn asks
for a one-line `[y/N]` confirmation before sending email, moving an item to
Trash, or overwriting an existing file. This confirmation runs only after
deterministic authorization has already passed, so it can only narrow behavior,
never grant a capability the request did not authorize. One-shot CLI runs and
image tasks have no terminal to answer, so those high-impact actions are denied
there rather than performed unconfirmed. See [SECURITY.md](SECURITY.md) for the
enforced threat boundary and explicit non-guarantees.

Mutation tools require authorization derived from the current user request.
File deletion uses Trash and is bound to an explicit filename, quoted name, or
path basename from the task. Rust independently checks authorization for
filesystem and artifact writes, email sending, attachment saving, overwriting,
and moving items to Trash; model instructions alone cannot grant those
capabilities.

When `FINN_TASK_LOG` is enabled, each task record includes a structured
authorization snapshot: derived capabilities, bound recipient, attachment, and
target counts, standard locations named by the user, untrusted-context state,
and the tool schemas exposed for the initial model round. Each recorded tool event also
includes its status, denial detail when applicable, and whether interactive
confirmation was required.

Each API round receives only the tool schemas authorized by the current user
request. The exposed set contracts after untrusted data enters the conversation,
while execution-time checks independently reject tool calls returned outside
that set. Every tool result is JSON-wrapped in a machine-generated
`untrusted_external_data` envelope before it is returned to the model.

Reads, writes, and attachments involving `.ssh`, `.gnupg`, AWS credentials,
shell startup files, Apple Mail storage, and macOS Keychains are
blocked, including access through resolvable symlinks. Task logs are off by
default; opted-in logs use user-only `0600` permissions.

Successful reads of filenames, files, artifacts, images, Codex output, or Mail
data activate a programmatic least-privilege mode for the rest of the session.
Mutations require explicit authorization in the current user request.
Instructions inside external content cannot grant those capabilities.

The mode remains active until Finn exits. Outbound mail is bound to email
addresses explicitly written in the current task. Outbound attachment names and
local file-content reads are likewise bound to file names in the current task;
directory inspection and writes can be scoped to Desktop, Documents, or
Downloads when that location is named. This intentionally rejects ambiguous
requests after Mail data has entered the session.

Files the current task itself creates — downloads, written documents,
transformed outputs — remain readable and writable for the remainder of that
task even when their names were chosen mid-task. Reading back an output the
task just produced reveals nothing new, and a file only enters this set
through a write that already passed authorization. The provenance set is
cleared when the task ends.

Email content still has to be sent to the configured model provider for language
processing. With the default GLM configuration, that means OpenRouter. Use a
provider and account whose data-handling terms are appropriate for the mail you
ask Finn to process.

## Artifact limits

Artifact processing is local and capped at 100 MB per input. DOCX/XLSX archives
also have entry-count, per-entry, and total expanded-size limits; image decoding
has dimension and allocation limits. Before DOCX/XLSX parsing, every XML entry
is scanned linearly and rejects tags above 64 KiB or 256 attributes. DOCX
creation is intended for straightforward reports and notes; complex layout editing is not a
replacement for Microsoft Word. DOCX text replacement preserves the package but
matches text within individual Word runs. PDF text replacement works only for
text represented by supported PDF text operations and fonts. XLSX formulas are
stored for recalculation by Excel or another spreadsheet application. Animated
images are currently processed as still images. Public HTTPS downloads have a
separate 25 MiB limit.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```
