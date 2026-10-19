//! Central multi-model workflow configuration and routing state.
//!
//! Reads `$CODEX_HOME/workflows.toml`, which maps symbolic model names to real
//! provider model identifiers and describes each workflow (`plan`, `code`,
//! `complex`, `debug`, `review`, `cto`, `cfo`, `auto`).
//!
//! This module is intentionally self-contained: it only *resolves* symbolic
//! names, validates reasoning levels, classifies task risk and persists plans.
//! Applying a selection to the running session stays in the TUI (`slash_dispatch`
//! and the plan-handoff path) so the agent execution loop is untouched.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use serde::Deserialize;
use serde::Serialize;

use crate::token_usage::TokenUsage;

/// File name of the central workflow configuration under `CODEX_HOME`.
pub(crate) const WORKFLOWS_FILE_NAME: &str = "workflows.toml";
/// Subdirectory under `CODEX_HOME` used to persist plans for recovery.
pub(crate) const PLANS_DIR_NAME: &str = "plans";

/// Shared transport definition for a workflow provider.
///
/// Parsed for documentation and validation; the live transport is still
/// configured by `~/.codex/config.toml`. Keeping the mapping here means the
/// provider details live in one place and can be asserted by tests.
#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct ProviderCfg {
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub wire_api: String,
}

/// Symbolic model definition: real provider identifier plus local metadata.
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct ModelCfg {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub input_price_per_token: f64,
    #[serde(default)]
    pub output_price_per_token: f64,
    /// Reasoning levels the provider accepts for this model, lowest to highest.
    #[serde(default)]
    pub supported_reasoning: Vec<String>,
}

/// One workflow's model/reasoning/permission assignment.
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct WorkflowCfg {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub description: String,
}

/// Deterministic risk signals for `/auto`.
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RoutingCfg {
    #[serde(default)]
    pub max_signals: Vec<String>,
    #[serde(default)]
    pub high_signals: Vec<String>,
}

/// The whole `workflows.toml` document.
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct WorkflowsConfig {
    #[serde(default)]
    #[allow(dead_code)]
    pub providers: BTreeMap<String, ProviderCfg>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelCfg>,
    #[serde(default)]
    pub workflows: BTreeMap<String, WorkflowCfg>,
    #[serde(default)]
    pub routing: RoutingCfg,
}

/// Path of the central workflow configuration file.
pub(crate) fn config_path_in(codex_home: &Path) -> PathBuf {
    codex_home.join(WORKFLOWS_FILE_NAME)
}

/// Parse `workflows.toml` from an explicit `CODEX_HOME`.
///
/// * `Ok(None)` - the file does not exist, so the workflow commands stay
///   inactive and Codex keeps its native behaviour.
/// * `Ok(Some(cfg))` - a parsed configuration.
/// * `Err(_)` - the file exists but cannot be read or parsed. The message is
///   returned verbatim so a broken configuration is reported instead of being
///   silently replaced by defaults.
///
/// The configuration is resolved from the session's `CODEX_HOME` rather than a
/// process-global lookup so tests and `--config` overrides stay deterministic.
pub(crate) fn load_from(codex_home: &Path) -> Result<Option<WorkflowsConfig>, String> {
    let path = config_path_in(codex_home);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("failed to read {}: {err}", path.display())),
    };
    toml::from_str(&contents)
        .map(Some)
        .map_err(|err| format!("failed to parse {}: {err}", path.display()))
}

/// Look up a workflow assignment by name (`plan`, `code`, ...).
pub(crate) fn workflow<'a>(
    cfg: &'a WorkflowsConfig,
    name: &str,
) -> Result<&'a WorkflowCfg, String> {
    cfg.workflows
        .get(name)
        .ok_or_else(|| format!("no [workflows.{name}] entry in {WORKFLOWS_FILE_NAME}"))
}

/// Resolve a symbolic model name (`deepseek`, `glm`) to its definition.
pub(crate) fn model<'a>(cfg: &'a WorkflowsConfig, symbolic: &str) -> Result<&'a ModelCfg, String> {
    cfg.models.get(symbolic).ok_or_else(|| {
        format!("no [models.{symbolic}] entry in {WORKFLOWS_FILE_NAME}")
    })
}

/// Find the symbolic name/definition that owns a real model identifier.
pub(crate) fn symbolic_for_model_id<'a>(
    cfg: &'a WorkflowsConfig,
    id: &str,
) -> Option<(&'a str, &'a ModelCfg)> {
    cfg.models
        .iter()
        .find(|(_, model)| model.id == id)
        .map(|(name, model)| (name.as_str(), model))
}

/// Validate a reasoning level against a model's declared support and convert it
/// to the protocol enum. Unsupported levels are an error, never substituted.
pub(crate) fn effort_for(model: &ModelCfg, level: &str) -> Result<ReasoningEffortConfig, String> {
    if !model
        .supported_reasoning
        .iter()
        .any(|supported| supported.eq_ignore_ascii_case(level))
    {
        return Err(format!(
            "model {} does not support reasoning '{level}' (supported: {})",
            model.id,
            model.supported_reasoning.join(", ")
        ));
    }
    ReasoningEffortConfig::from_str(level)
        .map_err(|err| format!("invalid reasoning '{level}': {err}"))
}

/// Rank used to pick the highest supported reasoning level.
fn effort_rank(level: &str) -> i32 {
    match level {
        "none" => 0,
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" => 4,
        "xhigh" => 5,
        "max" => 6,
        "ultra" => 7,
        "persistent" => 8,
        _ => -1,
    }
}

/// The model's highest declared reasoning level.
pub(crate) fn highest_effort(model: &ModelCfg) -> Result<ReasoningEffortConfig, String> {
    let level = model
        .supported_reasoning
        .iter()
        .max_by_key(|level| effort_rank(level))
        .ok_or_else(|| format!("model {} declares no supported reasoning levels", model.id))?;
    effort_for(model, level)
}

/// A resolved routing decision produced by the planner or the `/auto` classifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RoutingDecision {
    /// Symbolic model name (`deepseek` / `glm`).
    pub model_symbolic: String,
    /// Real provider model identifier.
    pub model_id: String,
    /// Reasoning level as it is written on the wire (`high` / `max`).
    pub effort: String,
    /// `routine` / `complex` / `difficult`.
    pub complexity: String,
    /// Human-readable rationale.
    pub reason: String,
}

impl RoutingDecision {
    /// The protocol enum for the selected effort.
    pub(crate) fn reasoning(&self) -> Result<ReasoningEffortConfig, String> {
        ReasoningEffortConfig::from_str(&self.effort)
            .map_err(|err| format!("invalid routing effort '{}': {err}", self.effort))
    }
}

/// Deterministic `/auto` classifier over the configured risk signals.
pub(crate) fn classify(cfg: &WorkflowsConfig, text: &str) -> RoutingDecision {
    let haystack = text.to_ascii_lowercase();
    let matched = |signals: &[String]| -> Vec<String> {
        signals
            .iter()
            .filter(|signal| haystack.contains(&signal.to_ascii_lowercase()))
            .cloned()
            .collect()
    };

    let max_hits = matched(&cfg.routing.max_signals);
    let high_hits = matched(&cfg.routing.high_signals);

    let (symbolic, effort, complexity, reason) = if !max_hits.is_empty() {
        (
            "glm",
            "max",
            "difficult",
            format!("hardware/unknown-cause signals: {}", max_hits.join(", ")),
        )
    } else if !high_hits.is_empty() {
        (
            "glm",
            "high",
            "complex",
            format!("architecture/integration signals: {}", high_hits.join(", ")),
        )
    } else {
        (
            "deepseek",
            "high",
            "routine",
            "no elevated risk signals detected".to_string(),
        )
    };

    let model_id = cfg
        .models
        .get(symbolic)
        .map(|model| model.id.clone())
        .unwrap_or_default();

    RoutingDecision {
        model_symbolic: symbolic.to_string(),
        model_id,
        effort: effort.to_string(),
        complexity: complexity.to_string(),
        reason,
    }
}

/// Permission snapshot captured before a workflow downgrades to read-only, so
/// `/code` can restore the user's own prior (never-elevated) choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavedPermissions {
    pub profile_id: String,
    pub display_label: String,
}

/// In-session workflow state carried on `ChatWidget`.
#[derive(Debug, Clone, Default)]
pub(crate) struct WorkflowState {
    /// Active workflow name (`plan`, `code`, `complex`, ...).
    pub workflow: Option<String>,
    /// Model/reasoning selected for the next implementation turn.
    pub routing: Option<RoutingDecision>,
    /// Most recent plan produced in Plan mode.
    pub plan_markdown: Option<String>,
    /// True while a plan is waiting to be implemented by `/code`.
    pub plan_ready: bool,
    /// True once the user has approved the current plan (native
    /// "implement this plan" prompt, or an explicit `/code` run). Distinct
    /// from `plan_ready`, which only means a plan was produced.
    pub plan_approved: bool,
    /// True when `/auto` routing is enabled.
    pub auto_enabled: bool,
    /// Permission snapshot captured by read-only workflows.
    pub saved_permissions: Option<SavedPermissions>,
    /// Consecutive unsuccessful fix cycles, used to recommend escalation.
    pub failed_attempts: u32,
    /// Set when a workflow command is about to submit its own message so the
    /// `/auto` classifier does not override the explicitly chosen model.
    pub suppress_auto_routing: bool,
}

/// Plan document persisted to disk for recovery across restarts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistedPlan {
    pub markdown: String,
    #[serde(default)]
    pub workflow: Option<String>,
    #[serde(default)]
    pub routing: Option<RoutingDecision>,
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub auto_enabled: bool,
}

/// Directory that holds persisted plan documents for a given `CODEX_HOME`.
pub(crate) fn plans_dir_in(codex_home: &Path) -> PathBuf {
    codex_home.join(PLANS_DIR_NAME)
}

/// Persist a plan (markdown plus routing) for the given thread id.
pub(crate) fn save_plan(
    codex_home: &Path,
    thread_id: &str,
    plan: &PersistedPlan,
) -> Result<(), String> {
    let dir = plans_dir_in(codex_home);
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("failed to create {}: {err}", dir.display()))?;
    let markdown_path = dir.join(format!("{thread_id}.md"));
    let json_path = dir.join(format!("{thread_id}.json"));
    std::fs::write(&markdown_path, &plan.markdown)
        .map_err(|err| format!("failed to write {}: {err}", markdown_path.display()))?;
    let json = serde_json::to_string_pretty(plan)
        .map_err(|err| format!("failed to serialize plan: {err}"))?;
    std::fs::write(&json_path, json)
        .map_err(|err| format!("failed to write {}: {err}", json_path.display()))?;
    Ok(())
}

/// Load a persisted plan for the given thread id, if one exists.
pub(crate) fn load_plan(codex_home: &Path, thread_id: &str) -> Option<PersistedPlan> {
    let dir = plans_dir_in(codex_home);
    let json = std::fs::read_to_string(dir.join(format!("{thread_id}.json"))).ok()?;
    serde_json::from_str(&json).ok()
}

/// Extract a planner-emitted routing JSON block from plan text.
///
/// The planner is asked to emit `{"coding_model": .., "reasoning": ..,
/// "complexity": .., "reason": ..}`. Symbolic names and real ids are both
/// accepted for `coding_model`.
pub(crate) fn parse_routing_block(cfg: &WorkflowsConfig, text: &str) -> Option<RoutingDecision> {
    let fence = text.find("```")?;
    let after = &text[fence + 3..];
    let body_start = after.find('\n').map(|idx| idx + 1).unwrap_or(0);
    let body = &after[body_start..];
    let end = body.find("```")?;
    let candidate = body[..end].trim();
    let json_start = candidate.find('{')?;
    let json_end = candidate.rfind('}')?;
    let value: serde_json::Value = serde_json::from_str(&candidate[json_start..=json_end]).ok()?;

    let coding_model = value.get("coding_model")?.as_str()?.trim();
    let reasoning = value
        .get("reasoning")
        .and_then(|value| value.as_str())
        .unwrap_or("high")
        .trim();
    let complexity = value
        .get("complexity")
        .and_then(|value| value.as_str())
        .unwrap_or("complex")
        .trim();
    let reason = value
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("planner-selected")
        .trim();

    let (symbolic, model) = if let Some(found) = cfg.models.get(coding_model) {
        (coding_model.to_string(), found)
    } else {
        let (name, model) = symbolic_for_model_id(cfg, coding_model)?;
        (name.to_string(), model)
    };

    Some(RoutingDecision {
        model_symbolic: symbolic,
        model_id: model.id.clone(),
        effort: reasoning.to_string(),
        complexity: complexity.to_string(),
        reason: reason.to_string(),
    })
}

/// Estimate a USD cost for a token usage snapshot using configured prices.
///
/// Returns `None` when the model is unknown or has no configured prices, so
/// missing pricing is never reported as zero.
pub(crate) fn estimate_cost_usd(
    cfg: &WorkflowsConfig,
    model_id: &str,
    usage: &TokenUsage,
) -> Option<f64> {
    let (_, model) = symbolic_for_model_id(cfg, model_id)?;
    if model.input_price_per_token <= 0.0 && model.output_price_per_token <= 0.0 {
        return None;
    }
    let input = usage.input_tokens.max(0) as f64 * model.input_price_per_token;
    let output = usage.output_tokens.max(0) as f64 * model.output_price_per_token;
    Some(input + output)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
[providers.openrouter]
base_url = "https://openrouter.ai/api/v1"
wire_api = "responses"

[models.deepseek]
id = "deepseek/deepseek-v4.1-flash"
provider = "openrouter"
input_price_per_token = 0.00000004
output_price_per_token = 0.000000144
supported_reasoning = ["none", "low", "high", "max"]

[models.glm]
id = "z-ai/glm-5.3"
provider = "openrouter"
input_price_per_token = 0.000000079
output_price_per_token = 0.0000025
supported_reasoning = ["low", "high", "max"]

[workflows.plan]
model = "deepseek"
reasoning = "max"
mode = "read-only"
description = "Plan"

[workflows.code]
model = "deepseek"
reasoning = "high"
mode = "coding"
description = "Code"

[workflows.debug]
model = "glm"
reasoning = "max"
mode = "coding"
description = "Debug"

[routing]
max_signals = ["usb", "bluetooth", "race", "barcode"]
high_signals = ["integration", "architecture", "migration"]
"#;

    fn cfg() -> WorkflowsConfig {
        toml::from_str(FIXTURE).expect("fixture parses")
    }

    #[test]
    fn effort_for_rejects_unsupported_levels_instead_of_substituting() {
        let cfg = cfg();
        let glm = model(&cfg, "glm").unwrap();
        assert!(effort_for(glm, "none").is_err());
        assert!(effort_for(glm, "xhigh").is_err());
        assert_eq!(
            effort_for(glm, "max").unwrap(),
            ReasoningEffortConfig::from_str("max").unwrap()
        );
        let deepseek = model(&cfg, "deepseek").unwrap();
        assert_eq!(
            effort_for(deepseek, "high").unwrap(),
            ReasoningEffortConfig::from_str("high").unwrap()
        );
    }

    #[test]
    fn highest_effort_uses_the_model_maximum() {
        let cfg = cfg();
        for symbolic in ["deepseek", "glm"] {
            let model = model(&cfg, symbolic).unwrap();
            assert_eq!(
                highest_effort(model).unwrap(),
                ReasoningEffortConfig::from_str("max").unwrap()
            );
        }
    }

    #[test]
    fn classify_routes_hardware_unknowns_to_glm_max() {
        let decision = classify(&cfg(), "Investigate intermittent Android USB scanner disconnections");
        assert_eq!(decision.model_symbolic, "glm");
        assert_eq!(decision.model_id, "z-ai/glm-5.3");
        assert_eq!(decision.effort, "max");
        assert!(decision.reason.contains("usb"));
    }

    #[test]
    fn classify_routes_integration_work_to_glm_high() {
        let decision = classify(&cfg(), "Refactor the service integration for billing");
        assert_eq!(decision.model_symbolic, "glm");
        assert_eq!(decision.effort, "high");
    }

    #[test]
    fn classify_defaults_to_deepseek_high() {
        let decision = classify(&cfg(), "Add a simple API endpoint");
        assert_eq!(decision.model_symbolic, "deepseek");
        assert_eq!(decision.model_id, "deepseek/deepseek-v4.1-flash");
        assert_eq!(decision.effort, "high");
        assert_eq!(decision.complexity, "routine");
    }

    #[test]
    fn parse_routing_block_accepts_symbolic_and_real_ids() {
        let symbolic = "Plan:\n\n```json\n{\"coding_model\": \"glm\", \"reasoning\": \"max\", \"complexity\": \"difficult\", \"reason\": \"usb lifecycle\"}\n```\n";
        let decision = parse_routing_block(&cfg(), symbolic).unwrap();
        assert_eq!(decision.model_id, "z-ai/glm-5.3");
        assert_eq!(decision.effort, "max");
        assert_eq!(decision.reason, "usb lifecycle");

        let real = "```json\n{\"coding_model\": \"deepseek/deepseek-v4.1-flash\", \"reasoning\": \"high\", \"complexity\": \"routine\", \"reason\": \"crud\"}\n```";
        let decision = parse_routing_block(&cfg(), real).unwrap();
        assert_eq!(decision.model_symbolic, "deepseek");
        assert_eq!(decision.effort, "high");
    }

    #[test]
    fn plan_round_trips_through_disk() {
        let thread_id = format!("workflow-models-test-{}", std::process::id());
        let plan = PersistedPlan {
            markdown: "# Plan\n\nDo the thing.".to_string(),
            workflow: Some("plan".to_string()),
            routing: parse_routing_block(
                &cfg(),
                "```json\n{\"coding_model\":\"glm\",\"reasoning\":\"high\",\"complexity\":\"complex\",\"reason\":\"x\"}\n```",
            ),
            approved: true,
            auto_enabled: true,
        };
        let home = tempfile::tempdir().expect("tempdir");
        save_plan(home.path(), &thread_id, &plan).expect("plan saves");
        let loaded = load_plan(home.path(), &thread_id).expect("plan loads");
        assert_eq!(loaded.markdown, plan.markdown);
        assert!(loaded.approved);
        assert!(loaded.auto_enabled);
        assert_eq!(loaded.routing.unwrap().model_id, "z-ai/glm-5.3");

        assert!(plans_dir_in(home.path()).join(format!("{thread_id}.md")).exists());
        assert!(plans_dir_in(home.path()).join(format!("{thread_id}.json")).exists());
    }

    #[test]
    fn cost_estimate_requires_known_pricing() {
        let cfg = cfg();
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            cached_input_tokens: 0,
            output_tokens: 1_000_000,
            reasoning_output_tokens: 0,
            total_tokens: 2_000_000,
        };
        let cost = estimate_cost_usd(&cfg, "z-ai/glm-5.3", &usage).expect("priced model");
        assert!((cost - (0.079 + 2.5)).abs() < 1e-6);
        assert!(estimate_cost_usd(&cfg, "unknown/model", &usage).is_none());
    }
}
