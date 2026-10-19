# Codex Multi-Model Workflow — Quick Guide

Your `codex` can switch model, reasoning effort and permissions **mid-session** from
slash commands. Same session, same conversation — no restart, no manual model picking.

## Everyday flow

```
/plan <task>   → investigate + plan (DeepSeek Max, read-only). Ends with a routing choice.
/code          → implement the approved plan (planner-chosen model, coding permissions)
/review        → check the changes (GLM High, read-only)
```

## Pick a model yourself

```
/complex   → GLM High       — complex integration / architecture work
/debug     → GLM Max        — hard root-cause debugging
/cto       → DeepSeek High  — technical research, architecture, docs
/cfo       → DeepSeek High  — spreadsheets, budgets, financial analysis
```

## Controls

```
/max           → raise current model to its max reasoning (keeps the model)
/effort        → show current effort + supported levels for the active model
/effort high   → set an exact supported level (invalid levels are rejected, never substituted)
/effort up|down→ step the effort up/down one level (Shift+Up / Shift+Down do the same)
/routing       → show the active OpenRouter routing variant + the available ones
/routing plain → switch variant in-session: plain / nitro / floor (also saved as the default)
/auto          → let Codex pick per task by risk (on/off)
/model-status  → workflow, model, provider, reasoning, routing, plan + approval, tokens & est. cost
```

## How model choice works

- `/plan` writes a structured routing decision (`coding_model`, `reasoning`,
  `complexity`, `reason`), so `/code` knows what to use.
- `/auto` uses deterministic risk signals:
  - hardware / USB / Bluetooth / RFID / barcode / scanner / Android / races /
    intermittent / flaky  → **GLM Max**
  - architecture / integration / migration / cross-service / state management →
    **GLM High**
  - otherwise → **DeepSeek High**
- If there is no plan, `/code` implements directly with DeepSeek High.

## Models available

| Symbol | OpenRouter base model ID | Used for |
|---|---|---|
| `deepseek` | `deepseek/deepseek-v4.1-flash` | planning, routine coding, CTO/CFO |
| `glm` | `z-ai/glm-5.3` | complex coding, debugging, review |

Both use OpenRouter (`https://openrouter.ai/api/v1`, Responses API). Reasoning levels
map to each model's real supported set; unsupported levels are rejected, not silently
substituted. `max` is the highest level both models accept.

Input modalities and list prices (verified against OpenRouter, 2026-10-19):

| Base model ID | Image input | In / M | Out / M |
|---|---|---|---|
| `deepseek/deepseek-v4.1-flash` | yes (text+image) | $0.033 | $0.080 |
| `z-ai/glm-5.3` | **no** (text only) | $0.079 | $1.400 |
| `z-ai/glm-5.3-flash` | yes (text+image+video) | $0.048 | $0.160 |

Image chat: `deepseek` is the default model and the cheapest image-capable option here, so
attaching an image in a normal session already lands on the right model. `glm` is text-only -
if you want GLM for images, add a `z-ai/glm-5.3-flash` entry to `[models.*]` in
`workflows.toml` (config only, no rebuild) and point a workflow at it.

These are list prices, not money spent; `/model-status` estimates cost from the per-model
prices in `workflows.toml`. Routing variant changes the real cost a lot: `nitro` is roughly
9x the DeepSeek plain rate (`/routing plain` or `/routing floor` brings it back down).

## Routing variants (cheap vs fast)

`workflows.toml` declares `[routing_variants]` (variant name -> model-id suffix). The active
variant is appended to each model's base id, so you can trade cost for speed without touching
the model assignments:

| Variant | Suffix | Trade-off |
|---|---|---|
| `plain` | *(none)* | Default OpenRouter endpoint - cheapest |
| `nitro` | `:nitro` | Fastest upstream host - can be several times more expensive |
| `floor` | `:floor` | Always pick the absolute cheapest endpoint |

```
/routing         → show active variant + available names
/routing plain   → switch in-session (no restart) and persist it as the default
/routing nitro   → back to fast
```

Switching re-points the live model id at the turn boundary, keeps the same session and effort,
and writes the choice to `[defaults].routing` so it survives a restart. Prices are per-variant
(`[models.<name>.variant_prices.<variant>]`) so `/model-status` cost stays accurate. An unknown
variant is a hard error listing the valid names - never silently substituted.

## Session affinity and provider routing (config.toml)

`[model_providers.openrouter]` carries two extra options, both config-driven:

* `send_session_id = true` - send the Codex session id as OpenRouter's `session_id` body field,
  so a conversation sticks to the same upstream host and keeps its prompt cache warm
  (`/model-status` shows `session_id on`).
* `[model_providers.openrouter.extra_body.provider]` - arbitrary OpenRouter `provider` routing
  preferences (e.g. `order`, `only`, `ignore`, `allow_fallbacks`, `sort`) merged verbatim into
  every request body. `/model-status` shows the active `extra_body`.

These are transport settings: edit `config.toml` and restart (they are not switchable mid-session,
because changing them would drop the warm upstream connection).


## Plans survive restarts

Plans and approval state are saved to `~/.codex/plans/`. Resume the session and you'll
see a "Recovered plan… Run /code to implement it" notice.

## Configuration (edit without recompiling)

`~/.codex/workflows.toml` holds all model / reasoning / mode mapping. Change
assignments there — no Rust rebuild needed.

Reasoning effort is configurable everywhere. A workflow's own `reasoning` wins,
otherwise `[defaults].reasoning` applies, otherwise the built-in `high`. The `/auto`
tiers are configurable per tier under `[routing]` (`max_reasoning`, `high_reasoning`,
`default_reasoning`, plus a `_model` for each); they ship at `low`. Unsupported levels
for a model are rejected, never silently downgraded.

OpenRouter routing is configurable too: `[routing_variants]` maps a variant name to a
model-id suffix and `[defaults].routing` picks the active one. Per-variant prices live in
`[models.<name>.variant_prices.<variant>]`; `/routing <name>` updates both the live model
and `[defaults].routing`.

## Notes / limitations

- `/debug` is not listed in the `/` popup (upstream `debug-config` namespace), but it
  works when typed.
- `/model-status` cost is a local estimate from `workflows.toml` pricing, not
  provider-reported billing.
- `/code`, `/complex`, `/cto`, `/cfo`, `/auto`, `/model-status` are new names;
  `/plan`, `/review`, `/max` extend the native commands.

Full details and the verified A–H acceptance-test table: see `MULTIMODEL.md`.
