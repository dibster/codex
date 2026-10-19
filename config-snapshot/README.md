# Codex configuration snapshot

The patched binary and all of this setup's configuration live **outside** the repo
(`~/.codex/...`). This folder is a redacted, restore-ready copy of the configuration so the
whole workflow can be rebuilt from the repository alone.

## Files and where they go

| Snapshot file | Restore to |
|---|---|
| `config.toml` | `~/.codex/config.toml` |
| `workflows.toml` | `~/.codex/workflows.toml` |
| `AGENTS.md` | `~/.codex/AGENTS.md` |
| `models.workflow.json` | `~/.codex/models.workflow.json` |
| `models.workflow.custom-entries.json` | reference only (the two custom catalog entries, extracted for readability) |

## Secrets

* `config.toml` has the inline OpenRouter key replaced with
  `REPLACE_WITH_YOUR_OPENROUTER_KEY` - paste a real key after restoring, or the OpenRouter
  requests will 401.
* The DeepSeek and Z.ai provider blocks use `env_key` (`DEEPSEEK_API_KEY`, `ZAI_API_KEY`) and
  contain no secret. They are only used if you point a model at those providers directly.
* `~/.codex/auth.json` (ChatGPT/OpenAI login) is deliberately **not** included. Run
  `codex login` instead.
* Session history, logs and `models_cache.json` are not included.

## Restore

1. Build or install the patched binary - see "Rebuild, install, rollback" in
   `../MULTIMODEL.md`, or drop one of the `codex.prefix-*` backups into
   `~/.codex/packages/standalone/current/bin/codex`.
2. Copy the four "Restore to" files above into `~/.codex/`.
3. Put a real OpenRouter key in `config.toml`.
4. `codex login` (native OpenAI models / MCP unaffected).
5. Verify: `codex --version` then `/model-status` inside a session - it should report
   workflow, model, provider, effort, routing variant and provider routing.

## What the config does

* Default provider is `openrouter` (`https://openrouter.ai/api/v1`, Responses API) with
  default model `deepseek/deepseek-v4.1-flash`, reasoning `low`; Plan mode uses `max`.
* `[model_providers.openrouter]` sets `send_session_id = true` (sticky upstream routing, keeps
  the prompt cache warm) and
  `[model_providers.openrouter.extra_body.provider] allow_fallbacks = true`.
* `workflows.toml` is the single source of truth for model assignments, per-model prices and
  supported reasoning levels, the `/auto` risk signals, and the OpenRouter routing variants.
  Edit it to change any of that - no rebuild needed.
* Native OpenAI models stay available with `codex --profile openai`.
