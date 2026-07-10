# FinnAgent Security Model

Finn is a single-user local macOS assistant. Model output and all content read
from files, filenames, images, Mail, and command output are untrusted.

## Enforced boundaries

- Each API round exposes only tools authorized by the current user request.
- Every returned tool call is checked again before execution.
- External tool results are JSON-wrapped as data and permanently taint the
  current session.
- After tainting, filesystem access is restricted to filenames or standard
  locations named in the current request.
- Outbound recipients and attachment filenames are bound to values explicitly
  written by the user.
- Trash operations are bound to an explicit filename, quoted name, or path
  basename written by the user.
- Overwrites, deletion, attachment saving, filesystem mutation, and artifact
  mutation require current-request authorization.
- High-impact, hard-to-reverse actions (email send, move to Trash, overwriting
  an existing file) require an additional interactive confirmation that runs
  only after authorization passes. Confirmation can only deny an already
  authorized action; it never grants one. Non-interactive runs (one-shot CLI,
  image tasks) deny these actions instead of performing them unconfirmed.
- Credential paths and resolvable symlinks into protected locations are denied.
- General shell execution is unavailable. Local actions use dedicated bounded
  tools; Codex delegation is separately authorized and sandboxed.
- Codex delegation is a separate, explicitly authorized capability. Codex runs
  in `workspace-write` mode inside a non-hidden directory below the user's
  home; symlink escapes are checked before workspace creation, provider API
  keys are removed, output is bounded and tainted, and only sessions started by
  the current Finn process may be resumed. Each session permits at most eight
  resume calls.
- Explicit web requests may expose OpenRouter's server-side web search and fetch
  tools to the selected model. Results and fetched page contents are untrusted,
  bounded external data; detected web-tool use taints the Finn session before
  local function calls execute. The requests run on OpenRouter infrastructure,
  not as unrestricted network access from the Mac.
- Mixed web/local tasks transition to a local-only model round after research.
  OpenRouter server-tool events are removed from the client function-call loop;
  local mutations remain limited to capabilities explicitly derived from the
  original user request.
- Task provenance: files a task itself creates through authorized writes stay
  readable and writable for the remainder of that task, even under
  untrusted-context restrictions. Reading back a task's own output reveals
  nothing the model did not already hold, and the provenance set is populated
  only from successful tool results, never from model claims. It is cleared
  when the task ends.
- OOXML inputs are scanned before parsing. XML tags larger than 64 KiB or with
  more than 256 attributes are rejected before upstream document libraries see
  the content.

## Non-guarantees

- Finn sends requested content to the configured model provider.
- Prompt injection can still influence wording or factual conclusions inside an
  action the user already authorized. Deterministic controls prevent capability
  expansion; they cannot prove semantic correctness. Authorization is inferred
  from natural language and is therefore not provably precise; the interactive
  confirmation gate and execution-time checks are the mitigations, not a proof
  of intent.
- Codex CLI remains an autonomous subprocess. Finn reviews completed JSONL
  turns and can resume them, but it does not relay interactive permission
  prompts while a Codex turn is running.
- OpenRouter web search and fetch are beta provider capabilities whose API,
  availability, source coverage, and pricing may change independently of Finn.
- Path canonicalization reduces symlink attacks but does not eliminate every
  possible local time-of-check/time-of-use race.
- Local malware running as the same macOS user is outside Finn's trust boundary.

Start a new Finn session when moving from untrusted-content review to an
unrelated task.

## Reporting

Do not include API keys, email contents, or private documents in a public issue.
Provide a minimal reproduction using synthetic data and identify the Finn
version shown by `finn --check`.
