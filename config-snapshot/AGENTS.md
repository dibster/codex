# Global Codex Behaviour

- Plan before substantial changes; for non-trivial work prefer `/plan` then `/code`.
- The planner selects the coding model and reasoning effort (see `~/.codex/workflows.toml`).
- Prefer the smallest correct change using existing patterns; avoid unnecessary abstractions.
- Keep modifications focused on the request.
- Respect repository conventions and existing configuration.
- Run appropriate tests after changes.
- Preserve acceptance criteria; do not rewrite tests just to make an implementation pass.
- Escalate unresolved complexity (after ~2 genuine failed fix cycles) rather than repeating the same fix.
- Avoid unnecessary agents; no routine parallel subagents.
