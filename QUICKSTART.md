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
/auto          → let Codex pick per task by risk (on/off)
/model-status  → workflow, model, provider, reasoning, plan + approval, tokens & est. cost
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

| Symbol | OpenRouter model ID | Used for |
|---|---|---|
| `deepseek` | `deepseek/deepseek-v4.1-flash` | planning, routine coding, CTO/CFO |
| `glm` | `z-ai/glm-5.3` | complex coding, debugging, review |

Both use OpenRouter (`https://openrouter.ai/api/v1`, Responses API). Reasoning levels
map to each model's real supported set; unsupported levels are rejected, not silently
substituted. `max` is the highest level both models accept.

## Plans survive restarts

Plans and approval state are saved to `~/.codex/plans/`. Resume the session and you'll
see a "Recovered plan… Run /code to implement it" notice.

## Configuration (edit without recompiling)

`~/.codex/workflows.toml` holds all model / reasoning / mode mapping. Change
assignments there — no Rust rebuild needed.

## Notes / limitations

- `/debug` is not listed in the `/` popup (upstream `debug-config` namespace), but it
  works when typed.
- `/model-status` cost is a local estimate from `workflows.toml` pricing, not
  provider-reported billing.
- `/code`, `/complex`, `/cto`, `/cfo`, `/auto`, `/model-status` are new names;
  `/plan`, `/review`, `/max` extend the native commands.

Full details and the verified A–H acceptance-test table: see `MULTIMODEL.md`.
