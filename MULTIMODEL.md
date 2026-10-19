# Codex Multi-Model Workflow (DeepSeek V4.1 Flash + GLM-5.3 via OpenRouter)

This documents a small, rebase-friendly patch to the upstream Rust `codex` TUI that adds
in-session model switching and a plan -> code workflow, driven entirely by slash commands.
All changes live under `codex-rs/tui/`; the agent execution loop is untouched.

## Commands

| Command | Model | Reasoning | Permissions | Purpose |
|---|---|---|---|---|
| `/plan [task]` | `deepseek/deepseek-v4.1-flash` | `max` | read-only | Investigate and produce an implementation plan; select the coding model |
| `/code [task]` | planner-selected | per routing | coding | Implement the approved plan (or code directly if none) |
| `/complex [task]` | `z-ai/glm-5.3` | `high` | coding | Manual override for complex implementation |
| `/debug [task]` | `z-ai/glm-5.3` | `max` | coding | Root-cause debugging of hard failures |
| `/review` | `z-ai/glm-5.3` | `high` | read-only | Review current changes against requirements |
| `/cto [task]` | `deepseek/deepseek-v4.1-flash` | `high` | read-only | Technical research, docs, architecture |
| `/cfo [task]` | `deepseek/deepseek-v4.1-flash` | `high` | read-only | Financial research, spreadsheets, budgets |
| `/max` | current model | highest supported | unchanged | Raise reasoning on the active model |
| `/effort [level\|up\|down]` | current model | shown/changed | unchanged | Show or change the active model's reasoning effort |
| `/routing [variant]` | current model | unchanged | unchanged | Show or change the OpenRouter routing variant (`plain`/`nitro`/`floor`) for the active model |
| `/auto` | classifier | per risk | unchanged | Enable automatic model selection |
| `/model-status` | - | - | - | Show workflow, model, provider, reasoning, plan, tokens, cost |

Examples:

```text
/plan Add a simple API endpoint
/code
/complex Refactor the billing service state machine
/debug Intermittent Android USB scanner disconnections
/review
/cto Compare our queue architecture with an event-log design
/cfo Reconcile the Q3 forecast against actuals
/max
/effort high
/effort up
/routing
/routing plain
/auto
/model-status
```

Every command also accepts an optional task after it, e.g. `/debug <task>` runs the switch and
submits the task in one step.

## Reasoning effort mapping

* Codex's canonical ladder is `none < minimal < low < medium < high < xhigh < max < ultra`.
* `/effort` (no argument) prints the active model, current effort and that model's supported
  levels; `/effort <level>` sets an exact supported level and hard-errors on anything else;
  `/effort up` / `/effort down` step one level. `Shift+Up` / `Shift+Down` (or `Alt+.` / `Alt+,`)
  do the same without the composer.
* `max` is a first-class variant in this fork and is emitted verbatim to OpenRouter.
* Workflow reasoning levels are validated against each model's `supported_reasoning` list from
  `workflows.toml`. An unsupported level is a hard error surfaced in the transcript - the code
  never silently substitutes a weaker level.
* Codex's `xhigh` is intentionally **not** used for these two models; `max` is their highest
  accepted level. If a workflow is configured with an unsupported level, `effort_for()` returns
  an error such as `model z-ai/glm-5.3 does not support reasoning 'xhigh'`.

## OpenRouter routing variants

`workflows.toml` declares `[routing_variants]`, a table mapping a variant name to a model-id
suffix. The active variant (default from `[defaults].routing`, shipped as `nitro`) is appended
to each configured model's **base** id at request time:

| Variant | Suffix | Trade-off |
|---|---|---|
| `plain` | *(empty)* | Default OpenRouter endpoint - cheapest, price-load-balanced |
| `nitro` | `:nitro` | Fastest upstream host(s) - can be several times more expensive |
| `floor` | `:floor` | Always the absolute cheapest endpoint |

`/routing` with no argument prints the active variant, the current model and the available
names; `/routing <name>` (case-insensitive) re-points the live model id at the next turn
boundary (same session, same effort), and persists the choice to `[defaults].routing` in
`workflows.toml` so it survives a restart. An unknown name is a hard error listing the valid
names - the variant is never silently substituted. `/model-status` shows the active variant.

Because base ids are stored without a suffix, `symbolic_for_model_id()` resolves suffixed ids
back to their symbolic model, and cost estimation is variant-aware via
`[models.<name>.variant_prices.<variant>]` (falling back to the model's base
`input_price_per_token`/`output_price_per_token`). The shipped config carries a `nitro`
override for both models, so `/model-status` cost stays accurate on the fast route.

## Provider transport

Both models use OpenRouter: `https://openrouter.ai/api/v1` with the Responses-compatible wire
API, matching `[model_providers.openrouter]` in `~/.codex/config.toml`. Because endpoint and
auth are shared, switching models inside a session is just a model-id change.

Custom model catalog entries must use `tool_mode = "direct"` (not `code_mode_only`) or the
shell tool breaks.

## Session affinity and provider routing

Two extra provider-body options are wired through `ModelProviderInfo` so OpenRouter routing is
config-driven, not hard-coded. Both are emitted as top-level fields on every
`/api/v1/responses` request by `build_responses_request`:

* `send_session_id = true` (in `[model_providers.openrouter]`) sends the Codex session id as the
  OpenRouter `session_id` body field. OpenRouter treats it as a **sticky-routing key**, so a
  conversation keeps hitting the same upstream provider between turns - which keeps that host's
  prompt cache warm. Cache hits surface as `usage.input_tokens_details.cached_tokens`. The flag
  is off by default because strict providers reject unknown body fields.
* `[model_providers.openrouter.extra_body]` is an arbitrary table merged verbatim into the
  request body. It is used for OpenRouter's `provider` routing preferences, e.g.:

  ```toml
  [model_providers.openrouter.extra_body.provider]
  allow_fallbacks = true
  order = ["DeepInfra/turbo"]
  # only = ["DeepInfra"]
  # ignore = ["Together"]
  # sort = "throughput"   # or "price" / "latency"
  ```

  These preferences are orthogonal to the `:nitro`/`:floor` **routing variants** above: the
  variant picks the OpenRouter sort strategy via the model-id suffix, while `provider` pins or
  excludes specific upstreams. Tool calls always use OpenRouter's Exacto routing.

Both options are exposed read-only in `/model-status` (`Provider routing: session_id on/off;
extra_body {...}`). Changing either is a `config.toml` edit plus a restart - unlike the routing
variant, they are provider-transport settings rather than per-turn workflow state, and must not
change mid-session because that would invalidate the warm upstream connection and cache.

## Configuration

Two files, both global (`$CODEX_HOME`, default `~/.codex`):

* `~/.codex/config.toml` - provider transport + auth (unchanged existing keys plus the
  OpenRouter provider block and model entries).
* `~/.codex/workflows.toml` - **the** central mapping: symbolic model names -> real provider
  base IDs, per-model prices (base + per-variant) and supported reasoning levels, each
  workflow's model/reasoning/mode, the `/auto` risk signals, the `[routing_variants]` table,
  and the global `[defaults]` (including the default `routing` variant). Edit this to change
  assignments; no recompile needed.

Every reasoning level is configurable, with a documented fallback chain:

* `[workflows.<name>].reasoning` - this workflow's level (highest priority).
* `[defaults].reasoning` - global fallback for workflows that omit `reasoning`.
* built-in `"high"` - used when both are absent.

`/auto` routing tiers are configurable too. `[routing]` holds the signal lists plus an
optional model and reasoning per tier - `max_model`/`max_reasoning`,
`high_model`/`high_reasoning`, `default_model`/`default_reasoning`. Each tier defaults to
its built-in (`glm`/`max`, `glm`/`high`, `deepseek`/`high`) when omitted, and the level is
validated against the model's `supported_reasoning` before it is applied. The shipped config
runs all three `/auto` tiers at `low` effort; raise a tier here (e.g. `max_reasoning = "max"`)
to change it without rebuilding.

Model identifiers are resolved in one place (`workflow_models.rs`) and never hard-coded across
modules.

`workflows.toml` is resolved from the session's `CODEX_HOME` (so `--config`/test overrides stay
deterministic). If the file is absent, the workflow commands stay inactive and Codex behaves
natively; if it is unreadable or malformed, the error is surfaced in the transcript rather than
silently ignored.

## Plan state and workflow

State machine: `IDLE -> PLANNING -> PLAN_READY -> APPROVED -> IMPLEMENTING -> VERIFYING ->
COMPLETE`, with `NEEDS_CLARIFICATION`, `BLOCKED`, `ESCALATION_REQUIRED`, `FAILED` as exceptions.

The planner emits a routing decision (a fenced JSON block in the plan):

```json
{"coding_model": "glm-5.3", "reasoning": "max", "complexity": "complex", "reason": "Android USB scanner lifecycle issue"}
```

`/code` reads that decision (or `/auto` classification, or the `[workflows.code]` default) and
applies the model, reasoning effort and coding permissions.

Plans are persisted for recovery at:

```text
~/.codex/plans/<thread_id>.json   # structured: markdown, routing, approved, auto_enabled
~/.codex/plans/<thread_id>.md     # human-readable markdown
```

On restart, resuming a thread restores the plan, approval state and selected model; `/model-status`
shows approval status.

## In-session switching and context

Model/reasoning/permission changes are applied at the turn boundary via the existing
`override_turn_context` / collaboration-mask APIs (`AppEvent::UpdateModel`,
`UpdateReasoningEffort`, permission-profile events). The visible session and workspace are the
same thread throughout - no new process or thread is spawned. `/code` injects the approved plan
as the next user message, so context is preserved by the runtime, not copied by hand.

Provider changes are safe because both models share the OpenRouter endpoint and auth.

## Cost and tokens

`/model-status` reports input (with cached), output and reasoning tokens from the provider usage
metadata, plus an **estimated** cost computed locally from the per-token prices in
`workflows.toml`. `(estimate)` marks it, and cost is reported unavailable (not zero) when the
model has no pricing configured.

## Verified acceptance tests

All checks below were run live against the installed patched binary (v0.162.0), in a single
running TUI session. Model identity was confirmed from the composer indicator, `/model-status`,
the routing/auto-routing transcript notices, and the wire request body.

| Test | Input | Observed result |
|---|---|---|
| A Planning | `/plan Add a simple API endpoint` | DeepSeek `max`, read-only, no files changed, plan written, routing `deepseek/high` |
| B Complex planning | `/plan Investigate intermittent Android USB scanner disconnections` | DeepSeek plans at `max`; routing `z-ai/glm-5.3 / max` citing usb/scanner/android/lifecycle |
| C Coding handoff | approve then `/code` | planner-selected model applied in the same session, approved plan retained, implementation begun |
| D Manual override | `/debug` | GLM-5.3 `max`, no restart, context preserved |
| E Review | `/review` | GLM-5.3 `high`, read-only; native review popup also opens (Esc closes it) |
| F CTO/CFO/Code | `/cto`, `/cfo`, `/complex` | DeepSeek `high`, DeepSeek `high`, GLM `high` respectively, same process |
| F Auto routing | `/auto` then a plain low-risk and high-risk prompt | low-risk -> `Auto routing: deepseek/deepseek-v4.1-flash (low)`; high-risk -> `Auto routing: z-ai/glm-5.3 (low) - hardware/... signals: usb, barcode, scanner, android, intermittent` (levels come from `[routing]`; shipped tiers are `low`) |
| G Recovery | `/plan ...`, approve, kill the TUI, `codex resume <id>` | plan + approval restored (`/model-status` -> `approved`), selected model restored, and the notice `Recovered plan for workflow 'plan' (<model>). Run /code to implement it.` is emitted |
| H Compatibility | native `/model`, `/review`, MCP, auth, streaming, shell + patch tools | all work; catalog lists both workflow models; real tool calls verified on both models |

Tip: the recovered-plan notice is inserted at the top of the transcript, so it sits above the
visible viewport after replay. Press `Ctrl+T` (or `F3` to search) to open the transcript overlay
and scroll to it; tmux/alternate-screen capture cannot see it.

## Known limitations

* `/debug` and other `debug*` entries are reserved in the upstream slash-command picker, so
  `/debug` works when typed but is hidden from the `/` popup list. This preserves the upstream
  `debug-config` namespace.
* `/review` opens the native review preset popup (by design) and also sets the review workflow.
* `/code` with no approved plan codes directly with DeepSeek `high` (the `[workflows.code]`
  default), as specified.
* `/plan` extends the native command: it still sets the native Plan collaboration mask (native
  `/plan` behaviour is preserved) and then starts the multi-model plan workflow.
* `/code`, `/complex`, `/cto`, `/cfo`, `/auto` and `/model-status` are new names and do not
  collide with any upstream command. `/max` and `/review` extend their existing behaviour.
* Upstream `cargo test -p codex-tui` has pre-existing, unrelated failures on a foreign
  `CODEX_HOME`: ~46 snapshot version-drift diffs, one test that stack-overflows unless run with
  `RUST_MIN_STACK=16777216`, and two known hanging tests. These reproduce on pristine upstream
  `rust-v0.162.0` and are not caused by this patch. The workflow unit tests pass:
  `RUST_MIN_STACK=16777216 cargo test -p codex-tui --lib workflow`.
* The installed binary is glibc-dynamic (built for `x86_64-unknown-linux-gnu`); the release
  directory name keeps the original `-musl` suffix so the AppArmor wildcard still matches.
* `codex-code-mode-host` is not rebuilt (unchanged upstream).

## Where everything lives

| What | Path |
|---|---|
| Fork (source) | `~/github/codex-multimodel`, branch `multimodel-workflow`, remote `fork` = `github.com:dibster/codex` (`origin` = upstream `openai/codex`) |
| Installed binary | `~/.codex/packages/standalone/current/bin/codex`, symlinked from `~/.local/bin/codex` (so `codex` on `PATH`) |
| Pre-patch backups | same dir: `codex.orig-0.162.0` (pristine upstream), `codex.prefix-*` (one per install step) |
| Provider transport + auth | `~/.codex/config.toml` |
| Workflow/model mapping | `~/.codex/workflows.toml` |
| Model catalog | `~/.codex/models.workflow.json` |
| Global instructions | `~/.codex/AGENTS.md` |
| Docs | this file (reference) and `QUICKSTART.md` (day-to-day) in the fork root |

Shell alias: `co` runs `codex --no-daemon --dangerously-bypass-approvals-and-sandbox` (no
daemon, approvals and sandbox bypassed). Plain `codex` uses the configured
`sandbox_mode`/`approval_policy`. Native OpenAI models remain available with
`codex --profile openai`.

Build environment on this box: `libcap-dev` is absent, so the sandbox (bwrap) build script
fails. Always build/test with:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CODEX_SKIP_BWRAP_BUILD=1
```

## Rebuild, install, rollback

Rebuild:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CODEX_SKIP_BWRAP_BUILD=1
cd ~/github/codex-multimodel/codex-rs
cargo build --release -p codex-cli
RUST_MIN_STACK=16777216 cargo test -p codex-tui --lib workflow
```

Install (atomic, preserving the original):

```bash
BIN=~/.codex/packages/standalone/current/bin
cp ~/github/codex-multimodel/codex-rs/target/release/codex "$BIN/codex.new"
strip "$BIN/codex.new"
mv "$BIN/codex.new" "$BIN/codex"
```

Rollback, one step (undo the last install):

```bash
BIN=~/.codex/packages/standalone/current/bin
ls "$BIN"/codex.prefix-*          # pick the one from before the change you want to undo
mv "$BIN/codex.prefix-sessionid-pre" "$BIN/codex"
```

Rollback, full (restore the untouched upstream binary - loses all workflow features):

```bash
BIN=~/.codex/packages/standalone/current/bin
mv "$BIN/codex.orig-0.162.0" "$BIN/codex"
```

Config rollback is just editing `~/.codex/config.toml` and `~/.codex/workflows.toml`; both
are read at session start, so no rebuild or reinstall is involved.

## Rebasing on upstream

* Fork base: `openai/codex` at `rust-v0.162.0`.
* Feature level - **TUI only** (workflow commands, model/reasoning switching, plan state):
  new files `codex-rs/tui/src/workflow_models.rs` and
  `codex-rs/tui/src/chatwidget/workflow.rs`; modified `slash_command.rs`,
  `slash_dispatch.rs`, `chatwidget.rs`, `constructor.rs`, `input_submission.rs`,
  `session_flow.rs`, `settings.rs`, `streaming.rs`, `turn_runtime.rs`,
  `app/event_dispatch.rs`, `lib.rs`.
* Transport level - **two additive provider-body options** (for OpenRouter session affinity
  and provider routing): `codex-rs/model-provider-info/src/lib.rs` (`send_session_id`,
  `extra_body` fields), `codex-rs/codex-api/src/common.rs` (`session_id` + flattened
  `extra_body` on the Responses request and its WebSocket mirror),
  `codex-rs/core/src/client.rs` (`build_responses_request` sets them;
  `responses_request_properties_match` treats a change as a connection reopen), and
  `codex-rs/config/src/thread_config/remote.rs` (ignores the two local-only fields, like
  `gateway_oauth`/`capabilities`). `Cargo.lock` picks up one dependency edge.
* The remaining touched files are mechanical: adding the two new fields (`false`/`None`) to
  existing `ModelProviderInfo` and `ResponsesApiRequest` struct literals in tests and support
  code. No behaviour change.
* No changes to the agent execution loop, the app-server protocol, auth, or MCP.
* `cargo build` and `cargo test` are unchanged; the upstream build/test process still
  applies (plus `CODEX_SKIP_BWRAP_BUILD=1` on this box).
