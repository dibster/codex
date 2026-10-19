//! Multi-model workflow commands layered on top of the existing TUI.
//!
//! Implements the `/plan`, `/code`, `/complex`, `/debug`, `/review`, `/cto`,
//! `/cfo`, `/max`, `/auto` and `/model-status` workflow commands. The module
//! only *selects* model, reasoning effort and permissions at a turn boundary and
//! submits the planner's saved plan as the next user message; the agent
//! execution loop, app-server protocol and provider transport are untouched.
//!
//! Model identifiers and reasoning levels come from `~/.codex/workflows.toml`
//! (see [`crate::workflow_models`]); nothing is hard-coded here.

use super::*;
use crate::app_event::AppEvent;
use crate::workflow_models;
use crate::workflow_models::SavedPermissions;

/// Coding/analysis workflows that are launched directly by a slash command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkflowJob {
    Code,
    Complex,
    Debug,
    Cto,
    Cfo,
}

impl WorkflowJob {
    fn workflow_name(self) -> &'static str {
        match self {
            WorkflowJob::Code => "code",
            WorkflowJob::Complex => "complex",
            WorkflowJob::Debug => "debug",
            WorkflowJob::Cto => "cto",
            WorkflowJob::Cfo => "cfo",
        }
    }

    /// Read-only jobs never elevate permissions, even if a save exists.
    fn read_only(self) -> bool {
        matches!(self, WorkflowJob::Cto | WorkflowJob::Cfo)
    }
}

impl ChatWidget {
    /// Resolve the workflow configuration from the session's `CODEX_HOME`.
    ///
    /// `Ok(Some(_))` when `workflows.toml` is present, `Ok(None)` when the file
    /// is absent (workflow commands stay inactive) and `Err(_)` when it is
    /// unreadable or malformed, so a broken configuration is surfaced rather
    /// than silently ignored. Reading from the session's `CODEX_HOME` (instead
    /// of a process-global lookup) keeps tests and `--config` overrides
    /// deterministic.
    fn load_workflow_config(
        &self,
    ) -> Result<Option<workflow_models::WorkflowsConfig>, String> {
        workflow_models::load_from(&self.config.codex_home)
    }

    /// Message shown when a workflow command runs without a `workflows.toml`.
    fn workflows_missing_message(&self) -> String {
        format!(
            "Multi-model workflows are not configured (no {} in {}).",
            workflow_models::WORKFLOWS_FILE_NAME,
            self.config.codex_home.display()
        )
    }

    /// Enter a workflow selected by a coding/analysis slash command.
    ///
    /// When `user_message` is supplied the task text is submitted under the new
    /// selection; otherwise the command only changes the active selection (and
    /// `/code` additionally submits the approved plan).
    pub(super) fn enter_workflow(
        &mut self,
        job: WorkflowJob,
        user_message: Option<UserMessage>,
    ) {
        let cfg = match self.load_workflow_config() {
            Ok(Some(cfg)) => cfg,
            Ok(None) => {
                self.add_error_message(self.workflows_missing_message());
                return;
            }
            Err(err) => {
                self.add_error_message(err);
                return;
            }
        };
        let name = job.workflow_name();
        let Ok(wf) = workflow_models::workflow(&cfg, name).cloned() else {
            self.add_error_message(
                workflow_models::workflow(&cfg, name)
                    .err()
                    .unwrap_or_default(),
            );
            return;
        };

        let (model_id, effort, reason) = match job {
            WorkflowJob::Code => match self.code_selection(&cfg, &wf) {
                Ok(selection) => selection,
                Err(err) => {
                    self.add_error_message(err);
                    return;
                }
            },
            _ => {
                let Ok(model) = workflow_models::model(&cfg, &wf.model).cloned() else {
                    self.add_error_message(
                        workflow_models::model(&cfg, &wf.model)
                            .err()
                            .unwrap_or_default(),
                    );
                    return;
                };
                let Ok(effort) = workflow_models::effort_for(&model, cfg.effort_level(&wf)) else {
                    self.add_error_message(
                        workflow_models::effort_for(&model, cfg.effort_level(&wf))
                            .err()
                            .unwrap_or_default(),
                    );
                    return;
                };
                (
                    model.id.clone(),
                    effort,
                    wf.description.clone(),
                )
            }
        };

        self.workflow.workflow = Some(name.to_string());

        // Permissions: read-only workflows always force read-only; coding
        // workflows restore the exact pre-plan permissions and never exceed
        // them. The `mode` in `workflows.toml` is authoritative, with the
        // job's built-in default as a fallback.
        let read_only = wf.mode.eq_ignore_ascii_case("read-only") || job.read_only();
        if read_only {
            self.snapshot_permissions_if_absent();
            if !self.apply_permission_preset_id("read-only") {
                return;
            }
        } else if !self.restore_saved_permissions() {
            return;
        }

        if let Err(err) = self.apply_model_selection(&model_id, effort) {
            self.add_error_message(err);
            return;
        }

        let mut message = user_message;
        if message.is_none() && job == WorkflowJob::Code {
            message = self.take_plan_implementation_message();
        }

        match message {
            Some(message) => {
                self.add_info_message(
                    format!(
                        "Implementing with {model_id} ({} reasoning).",
                        cfg.effort_level(&wf)
                    ),
                    /*hint*/ None,
                );
                self.workflow.suppress_auto_routing = true;
                self.submit_workflow_message(message);
            }
            None => {
                let hint = if job == WorkflowJob::Code {
                    Some("Run /plan first, or describe the task after /code.".to_string())
                } else {
                    Some(format!("/{} <task> to run it now.", name))
                };
                self.add_info_message(
                    format!(
                        "Workflow '/{name}' active: {model_id} with {} reasoning. {reason}",
                        cfg.effort_level(&wf)
                    ),
                    hint,
                );
            }
        }
    }

    /// Read-only planning selection. Returns `false` when the workflow
    /// configuration cannot be resolved (caller aborts the command).
    pub(super) fn begin_plan_workflow(&mut self) -> bool {
        let cfg = match self.load_workflow_config() {
            Ok(Some(cfg)) => cfg,
            Ok(None) => {
                // No `workflows.toml`: native plan mode still applies, so this
                // is a silent no-op rather than an error.
                return false;
            }
            Err(err) => {
                self.add_error_message(err);
                return false;
            }
        };
        let Ok(wf) = workflow_models::workflow(&cfg, "plan").cloned() else {
            self.add_error_message(
                workflow_models::workflow(&cfg, "plan")
                    .err()
                    .unwrap_or_default(),
            );
            return false;
        };
        let Ok(model) = workflow_models::model(&cfg, &wf.model).cloned() else {
            self.add_error_message(
                workflow_models::model(&cfg, &wf.model)
                    .err()
                    .unwrap_or_default(),
            );
            return false;
        };
        let Ok(effort) = workflow_models::effort_for(&model, cfg.effort_level(&wf)) else {
            self.add_error_message(
                workflow_models::effort_for(&model, cfg.effort_level(&wf))
                    .err()
                    .unwrap_or_default(),
            );
            return false;
        };

        self.workflow.workflow = Some("plan".to_string());
        self.workflow.routing = None;
        self.workflow.plan_markdown = None;
        self.workflow.plan_ready = false;
        self.workflow.plan_approved = false;
        // The planning turn must never be hijacked by `/auto`: the planner
        // itself selects the coding model for the later `/code` step.
        self.workflow.suppress_auto_routing = true;
        self.snapshot_permissions_if_absent();
        if !self.apply_permission_preset_id("read-only") {
            return false;
        }
        if let Err(err) = self.apply_plan_model_selection(&model.id, effort) {
            self.add_error_message(err);
            return false;
        }
        self.add_info_message(
            format!(
                "Planning with {} ({} reasoning, read-only). The planner selects the coding model for /code.",
                model.id, cfg.effort_level(&wf)
            ),
            /*hint*/ None,
        );
        true
    }

    /// `/review`: GLM High, read-only, preserving native review behaviour.
    pub(super) fn enter_review_workflow(&mut self) {
        let cfg = match self.load_workflow_config() {
            Ok(Some(cfg)) => cfg,
            Ok(None) => return,
            Err(err) => {
                self.add_error_message(err);
                return;
            }
        };
        let Ok(wf) = workflow_models::workflow(&cfg, "review").cloned() else {
            return;
        };
        let Ok(model) = workflow_models::model(&cfg, &wf.model).cloned() else {
            return;
        };
        let Ok(effort) = workflow_models::effort_for(&model, cfg.effort_level(&wf)) else {
            return;
        };
        self.workflow.workflow = Some("review".to_string());
        self.snapshot_permissions_if_absent();
        if !self.apply_permission_preset_id("read-only") {
            return;
        }
        let _ = self.apply_model_selection(&model.id, effort);
    }

    /// `/max`: raise the current model to its highest supported reasoning.
    pub(super) fn apply_max_reasoning(&mut self) {
        let model_id = self.current_model().to_string();
        let mut applied: Option<ReasoningEffortConfig> = None;

        if let Ok(Some(cfg)) = self.load_workflow_config()
            && let Some((_symbolic, model)) =
                workflow_models::symbolic_for_model_id(&cfg, &model_id)
            && let Ok(effort) = workflow_models::highest_effort(model)
        {
            applied = Some(effort);
        }

        if applied.is_none() {
            applied = self.catalog_highest_effort(&model_id);
        }

        let Some(effort) = applied else {
            self.add_error_message(format!(
                "No supported reasoning levels are known for model {model_id}."
            ));
            return;
        };

        self.apply_reasoning_only(effort.clone());
        self.add_info_message(
            format!("Reasoning raised to {} for {model_id}.", effort.as_str()),
            /*hint*/ None,
        );
    }

    /// `/effort`: show or change the active model's reasoning effort.
    ///
    /// With no argument it reports the current effort and the levels the active
    /// model advertises. `up`/`down` step through those advertised levels; any
    /// other argument is validated against them and rejected when unsupported,
    /// never silently substituted.
    pub(super) fn apply_effort_command(&mut self, arg: &str) {
        let model_id = self.current_model().to_string();
        let choices = self.supported_efforts(&model_id);
        let supported = choices
            .iter()
            .map(|effort| effort.as_str().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let current = self.effective_reasoning_effort();

        let requested = arg.trim();
        if requested.is_empty() {
            let current_label = current.as_ref().map_or_else(
                || "default".to_string(),
                |effort| effort.as_str().to_string(),
            );
            self.add_info_message(
                format!(
                    "Reasoning effort: {current_label}\nModel: {model_id}\nSupported: {supported}\nUsage: /effort <level> | up | down"
                ),
                /*hint*/ None,
            );
            return;
        }

        let normalized = requested.to_ascii_lowercase();
        let anchor = current
            .clone()
            .filter(|effort| choices.contains(effort))
            .or_else(|| {
                self.catalog_default_effort(&model_id)
                    .filter(|effort| choices.contains(effort))
            })
            .or_else(|| choices.first().cloned());

        let target = match normalized.as_str() {
            "up" | "raise" | "+" => step_effort(&choices, anchor.as_ref(), /*raise*/ true),
            "down" | "lower" | "-" => step_effort(&choices, anchor.as_ref(), /*raise*/ false),
            _ => match choices
                .iter()
                .find(|effort| effort.as_str().eq_ignore_ascii_case(&normalized))
            {
                Some(effort) => Some(effort.clone()),
                None => {
                    self.add_error_message(format!(
                        "Model {model_id} does not support reasoning '{requested}'. Supported: {supported}."
                    ));
                    return;
                }
            },
        };

        let Some(effort) = target else {
            self.add_info_message(
                format!("Reasoning is already at the boundary of what {model_id} supports ({supported})."),
                /*hint*/ None,
            );
            return;
        };

        self.apply_reasoning_only(effort.clone());
        self.add_info_message(
            format!(
                "Reasoning effort set to {} for {model_id}.",
                effort.as_str()
            ),
            /*hint*/ None,
        );
    }

    /// Advertised reasoning levels for a model, ordered lowest to highest.
    fn supported_efforts(&self, model_id: &str) -> Vec<ReasoningEffortConfig> {
        let mut efforts = self.configured_supported_efforts(model_id);
        if efforts.is_empty() {
            efforts = self.catalog_supported_efforts(model_id);
        }
        efforts.sort_by_key(|effort| reasoning_rank(effort.as_str()));
        efforts.dedup();
        efforts
    }

    /// Reasoning levels declared for a model in `workflows.toml`.
    fn configured_supported_efforts(&self, model_id: &str) -> Vec<ReasoningEffortConfig> {
        let Ok(Some(cfg)) = self.load_workflow_config() else {
            return Vec::new();
        };
        let Some((_symbolic, model)) = workflow_models::symbolic_for_model_id(&cfg, model_id)
        else {
            return Vec::new();
        };
        model
            .supported_reasoning
            .iter()
            .filter_map(|level| workflow_models::effort_for(model, level).ok())
            .collect()
    }

    /// Reasoning levels advertised by the model catalog.
    fn catalog_supported_efforts(&self, model_id: &str) -> Vec<ReasoningEffortConfig> {
        let models = self.model_catalog.try_list_models().unwrap_or_default();
        models
            .into_iter()
            .find(|preset| preset.model == model_id)
            .map(|preset| {
                preset
                    .supported_reasoning_efforts
                    .iter()
                    .map(|option| option.effort.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Default reasoning level advertised by the model catalog, when known.
    fn catalog_default_effort(&self, model_id: &str) -> Option<ReasoningEffortConfig> {
        let models = self.model_catalog.try_list_models().ok()?;
        let preset = models.into_iter().find(|preset| preset.model == model_id)?;
        Some(preset.default_reasoning_effort.clone())
    }

    /// `/auto`: enable classifier-based model selection for subsequent tasks.
    pub(super) fn enable_auto_routing(&mut self) {
        self.workflow.auto_enabled = true;
        self.add_info_message(
            "Automatic model selection enabled. /code routes by task risk; /plan routing uses the classifier when the planner omits a routing block.".to_string(),
            /*hint*/ None,
        );
    }

    /// `/auto`: classify a plain user prompt and select the matching model.
    ///
    /// Only runs when automatic routing is enabled and the planner has not
    /// already produced a routing decision for the next implementation turn.
    pub(super) fn maybe_auto_route(&mut self, text: &str) {
        if !self.workflow.auto_enabled || self.workflow.routing.is_some() {
            return;
        }
        if self.workflow.suppress_auto_routing {
            self.workflow.suppress_auto_routing = false;
            return;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with('!') {
            return;
        }
        let Ok(Some(cfg)) = self.load_workflow_config() else {
            return;
        };
        let decision = workflow_models::classify(&cfg, trimmed);
        let Ok(model) = workflow_models::model(&cfg, &decision.model_symbolic) else {
            return;
        };
        let Ok(effort) = workflow_models::effort_for(model, &decision.effort) else {
            return;
        };
        if decision.model_id == self.current_model()
            && self.effective_reasoning_effort().as_ref() == Some(&effort)
        {
            return;
        }
        if self.apply_model_selection(&decision.model_id, effort).is_ok() {
            self.add_info_message(
                format!(
                    "Auto routing: {} ({}) \u{2013} {}",
                    decision.model_id, decision.effort, decision.reason
                ),
                /*hint*/ None,
            );
        }
    }

    /// Restore a persisted plan when resuming a thread after a restart.
    pub(super) fn restore_persisted_workflow(&mut self) {
        let Some(thread_id) = self.thread_id else {
            return;
        };
        let Some(plan) =
            workflow_models::load_plan(&self.config.codex_home, &thread_id.to_string())
        else {
            return;
        };
        let had_routing = plan.routing.is_some();
        self.workflow.workflow = plan.workflow.clone();
        self.workflow.routing = plan.routing.clone();
        self.workflow.plan_markdown = Some(plan.markdown.clone());
        self.workflow.plan_ready = plan.approved || !plan.markdown.trim().is_empty();
        self.workflow.plan_approved = plan.approved;
        self.workflow.auto_enabled = plan.auto_enabled;
        self.workflow.failed_attempts = 0;
        if plan.approved || had_routing {
            let model = plan
                .routing
                .as_ref()
                .map(|routing| routing.model_id.clone())
                .unwrap_or_else(|| "model not yet selected".to_string());
            self.add_info_message(
                format!(
                    "Recovered plan for workflow '{}' ({model}). Run /code to implement it.",
                    plan.workflow.unwrap_or_else(|| "plan".to_string())
                ),
                Some("Recovered from ~/.codex/plans/.".to_string()),
            );
        }
    }

    /// `/model-status`: show the current workflow, model, plan and usage.
    pub(super) fn add_workflow_status_output(&mut self) {
        let model_id = self.current_model().to_string();
        let workflow = self
            .workflow
            .workflow
            .clone()
            .unwrap_or_else(|| "idle".to_string());
        let effort = self
            .effective_reasoning_effort()
            .map(|effort| effort.as_str().to_string())
            .unwrap_or_else(|| "default".to_string());
        let provider = self
            .load_workflow_config()
            .ok()
            .flatten()
            .and_then(|cfg| {
                workflow_models::symbolic_for_model_id(&cfg, &model_id)
                    .map(|(_, model)| model.provider.clone())
            })
            .filter(|provider| !provider.is_empty())
            .unwrap_or_else(|| self.config.model_provider_id.clone());

        let plan_state = if self.workflow.plan_approved {
            "approved (run /code to implement)"
        } else if self.workflow.plan_ready {
            "ready (awaiting approval)"
        } else if self.workflow.plan_markdown.is_some() {
            "captured"
        } else {
            "none"
        };
        let routing = self
            .workflow
            .routing
            .as_ref()
            .map(|routing| {
                format!(
                    "{} ({}) \u{2013} {}",
                    routing.model_id, routing.effort, routing.reason
                )
            })
            .unwrap_or_else(|| "not yet selected".to_string());

        let mut lines = vec![
            format!("Workflow: {workflow}"),
            format!("Model: {model_id}"),
            format!("Provider: {provider}"),
            format!("Reasoning effort: {effort}"),
            format!("Auto routing: {}", if self.workflow.auto_enabled { "on" } else { "off" }),
            format!("Plan: {plan_state}"),
            format!("Planner routing: {routing}"),
        ];

        if let Some(info) = self.token_info.as_ref() {
            let usage = &info.total_token_usage;
            lines.push(format!(
                "Tokens (total): input {} (cached {}), output {}, reasoning {}",
                usage.input_tokens,
                usage.cached_input_tokens,
                usage.output_tokens,
                usage.reasoning_output_tokens
            ));
            match self.load_workflow_config().ok().flatten().and_then(|cfg| {
                workflow_models::estimate_cost_usd(&cfg, &model_id, usage)
            }) {
                Some(cost) => {
                    lines.push(format!("Estimated cost: ${cost:.4} (estimate)"));
                }
                None => {
                    lines.push(
                        "Estimated cost: unavailable (no pricing for this model)".to_string(),
                    );
                }
            }
        } else {
            lines.push("Tokens: no usage reported yet".to_string());
        }

        self.add_info_message(lines.join("\n"), /*hint*/ None);
    }

    // ---- internals -------------------------------------------------------

    /// Choose the coding selection for `/code`: planner routing first, then
    /// `/auto` classification, then the `[workflows.code]` default.
    fn code_selection(
        &self,
        cfg: &workflow_models::WorkflowsConfig,
        wf: &workflow_models::WorkflowCfg,
    ) -> Result<(String, ReasoningEffortConfig, String), String> {
        if let Some(routing) = &self.workflow.routing {
            let model = workflow_models::model(cfg, &routing.model_symbolic)?;
            let effort = workflow_models::effort_for(model, &routing.effort)?;
            return Ok((routing.model_id.clone(), effort, routing.reason.clone()));
        }

        let default_model = workflow_models::model(cfg, &wf.model)?;
        let default_effort = workflow_models::effort_for(default_model, cfg.effort_level(wf))?;

        if !self.workflow.auto_enabled {
            return Ok((
                default_model.id.clone(),
                default_effort,
                wf.description.clone(),
            ));
        }

        let text = self
            .workflow
            .plan_markdown
            .clone()
            .unwrap_or_default();
        let decision = workflow_models::classify(cfg, &text);
        let model = workflow_models::model(cfg, &decision.model_symbolic)?;
        let effort = workflow_models::effort_for(model, &decision.effort)?;
        Ok((decision.model_id, effort, decision.reason))
    }

    /// Build and submit the approved plan as the next user message.
    fn take_plan_implementation_message(&mut self) -> Option<UserMessage> {
        if self.workflow.plan_markdown.is_none()
            && let Some(thread_id) = self.thread_id
            && let Some(plan) =
                workflow_models::load_plan(&self.config.codex_home, &thread_id.to_string())
        {
            self.workflow.plan_markdown = Some(plan.markdown.clone());
            self.workflow.plan_ready = true;
            self.workflow.plan_approved = plan.approved;
            self.workflow.routing = plan.routing;
            self.workflow.auto_enabled |= plan.auto_enabled;
        }
        let text = self.workflow.plan_markdown.clone()?;
        // Consuming the plan for implementation is an explicit approval act.
        self.workflow.plan_approved = true;
        self.persist_plan();
        Some(UserMessage {
            text,
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        })
    }

    fn submit_workflow_message(&mut self, message: UserMessage) {
        if self.is_session_configured() {
            self.reasoning_buffer.clear();
            self.reasoning_header = None;
            self.reasoning_summary_parts.clear();
            self.set_status_header(String::from("Working"));
            self.submit_user_message_with_shell_escape_policy(
                message,
                ShellEscapePolicy::Disallow,
            );
        } else {
            self.queue_user_message_with_options(
                message,
                QueuedInputAction::ParseSlash,
                Vec::new(),
            );
        }
    }

    /// Apply a model + effort while staying in Default (coding) mode.
    fn apply_model_selection(
        &mut self,
        model_id: &str,
        effort: ReasoningEffortConfig,
    ) -> Result<(), String> {
        let mut mask = crate::collaboration_modes::default_mode_mask(self.model_catalog.as_ref());
        if let Some(mask) = mask.as_mut() {
            mask.model = Some(model_id.to_string());
            mask.reasoning_effort = Some(Some(effort.clone()));
        }
        match mask {
            Some(mask) => self.set_collaboration_mask_from_user_action(mask),
            None => self.set_reasoning_effort(Some(effort.clone())),
        }
        self.set_model(model_id);
        self.app_event_tx
            .send(AppEvent::UpdateModel(model_id.to_string()));
        self.app_event_tx
            .send(AppEvent::UpdateReasoningEffort(Some(effort)));
        Ok(())
    }

    /// Apply a model + effort while entering Plan mode.
    fn apply_plan_model_selection(
        &mut self,
        model_id: &str,
        effort: ReasoningEffortConfig,
    ) -> Result<(), String> {
        self.set_plan_mode_reasoning_effort(Some(effort.clone()));
        let mut mask = crate::collaboration_modes::plan_mask(self.model_catalog.as_ref())
            .or_else(|| self.active_collaboration_mask.clone());
        let Some(mask) = mask.as_mut() else {
            return Err("Plan mode is unavailable right now.".to_string());
        };
        mask.model = Some(model_id.to_string());
        mask.reasoning_effort = Some(Some(effort.clone()));
        self.set_collaboration_mask_from_user_action(mask.clone());
        self.app_event_tx
            .send(AppEvent::UpdateModel(model_id.to_string()));
        Ok(())
    }

    /// Change only the reasoning effort, preserving the active mode's model.
    fn apply_reasoning_only(&mut self, effort: ReasoningEffortConfig) {
        if self.active_mode_kind() == ModeKind::Plan {
            self.set_plan_mode_reasoning_effort(Some(effort.clone()));
        } else {
            self.set_reasoning_effort(Some(effort.clone()));
        }
        self.app_event_tx
            .send(AppEvent::UpdateReasoningEffort(Some(effort)));
    }

    fn catalog_highest_effort(&self, model_id: &str) -> Option<ReasoningEffortConfig> {
        let models = self.model_catalog.try_list_models().ok()?;
        let preset = models.into_iter().find(|preset| preset.model == model_id)?;
        preset
            .supported_reasoning_efforts
            .iter()
            .max_by_key(|preset| reasoning_rank(preset.effort.as_str()))
            .map(|preset| preset.effort.clone())
    }

    pub(super) fn snapshot_permissions_if_absent(&mut self) {
        if self.workflow.saved_permissions.is_some() {
            return;
        }
        // Capture the raw approval policy and canonical permission profile, not
        // just the named preset. Sessions launched with
        // `--dangerously-bypass-approvals-and-sandbox` (YOLO) have no named
        // active profile, so without the raw state there would be nothing to
        // restore and `/code` would be stuck in the forced read-only mode.
        let approval_policy =
            AskForApproval::from(self.config.permissions.approval_policy.value());
        let permission_profile = self.config.permissions.permission_profile().clone();
        let active = self.config.permissions.active_permission_profile();
        let label = active
            .as_ref()
            .and_then(|active| {
                builtin_approval_presets()
                    .into_iter()
                    .find(|preset| {
                        preset.active_permission_profile.id == active.id || preset.id == active.id
                    })
                    .map(|preset| preset.label.to_string())
            })
            .or_else(|| {
                preset_matching(&approval_policy, &permission_profile)
                    .map(|preset| preset.label.to_string())
            })
            .unwrap_or_else(|| "Current permissions".to_string());
        self.workflow.saved_permissions = Some(SavedPermissions {
            profile_id: active.map(|active| active.id.clone()),
            display_label: label,
            approval_policy,
            permission_profile,
        });
    }

    /// Restore the exact permissions captured before planning. Never elevates:
    /// if no snapshot or matching preset exists, permissions are left unchanged.
    pub(crate) fn restore_saved_permissions(&mut self) -> bool {
        if let Some(saved) = self.workflow.saved_permissions.clone() {
            // Prefer restoring the named profile identity when one was captured.
            if let Some(profile_id) = saved.profile_id.as_deref()
                && self.apply_permission_preset_id(profile_id)
            {
                return true;
            }
            // Unnamed sessions (e.g. YOLO) have no profile id. Match the raw
            // approval policy and permission profile against the built-in
            // presets so the exact prior state is restored.
            if let Some(preset) = preset_matching(&saved.approval_policy, &saved.permission_profile)
                && self.apply_permission_preset_id(preset.id)
            {
                return true;
            }
        }
        // No snapshot (e.g. `/code` without `/plan`): keep the current preset.
        let current = self.config.permissions.active_permission_profile();
        if let Some(active) = current
            && self.apply_permission_preset_id(&active.id)
        {
            return true;
        }
        // Nothing matched; leave the active permissions untouched.
        true
    }

    fn apply_permission_preset_id(&self, preset_id: &str) -> bool {
        // Accept either the stable preset id ("read-only") or the built-in
        // permission-profile id (":read-only"), so callers can pass whichever
        // identifier they hold. Never falls back to a different preset.
        let Some(preset) = builtin_approval_presets().into_iter().find(|preset| {
            preset.id == preset_id || preset.active_permission_profile.id == preset_id
        }) else {
            return false;
        };
        let approval = AskForApproval::from(preset.approval);
        let reviewer = self.config.approvals_reviewer;
        self.app_event_tx
            .send(AppEvent::CodexOp(AppCommand::override_turn_context(
                /*cwd*/ None,
                Some(approval),
                Some(reviewer),
                Some(preset.permission_profile.clone()),
                Some(preset.active_permission_profile.clone()),
                /*model*/ None,
                /*effort*/ None,
                /*summary*/ None,
                /*service_tier*/ None,
                /*collaboration_mode*/ None,
            )));
        self.app_event_tx
            .send(AppEvent::UpdateAskForApprovalPolicy(approval));
        self.app_event_tx
            .send(AppEvent::UpdateActivePermissionProfile(
                preset.active_permission_profile.clone(),
            ));
        self.app_event_tx
            .send(AppEvent::UpdateApprovalsReviewer(reviewer));
        true
    }

    /// Apply the stored routing decision to the native plan-implementation
    /// prompt so approving the plan also selects the right coding model.
    pub(super) fn apply_routing_to_mask(&self, mask: &mut CollaborationModeMask) {
        let Some(routing) = &self.workflow.routing else {
            return;
        };
        if let Ok(effort) = routing.reasoning() {
            mask.model = Some(routing.model_id.clone());
            mask.reasoning_effort = Some(Some(effort));
        }
    }

    /// Mark the pending plan as explicitly approved (native "implement this
    /// plan" prompt). Keeps `plan_ready` true and records approval on disk.
    pub(crate) fn approve_active_plan(&mut self) {
        if self.workflow.plan_ready {
            self.workflow.plan_approved = true;
            self.persist_plan();
        }
    }

    /// Record a completed plan, classify its routing and persist it.
    pub(super) fn record_completed_plan(&mut self, markdown: &str) {
        if markdown.trim().is_empty() {
            return;
        }
        // Replaying a resumed transcript re-streams the previous plan. That
        // must not be mistaken for a freshly produced plan, or the recorded
        // approval would be cleared on every restart.
        let persisted = self.thread_id.and_then(|thread_id| {
            workflow_models::load_plan(&self.config.codex_home, &thread_id.to_string())
        });
        let is_replay = self
            .workflow
            .plan_markdown
            .as_deref()
            .is_some_and(|existing| existing.trim() == markdown.trim())
            || persisted
                .as_ref()
                .is_some_and(|plan| plan.markdown.trim() == markdown.trim());
        self.workflow.plan_markdown = Some(markdown.to_string());
        self.workflow.plan_ready = true;
        // A newly produced plan awaits explicit approval; a replayed plan keeps
        // whatever approval is already recorded.
        self.workflow.plan_approved = if is_replay {
            self.workflow.plan_approved || persisted.as_ref().is_some_and(|plan| plan.approved)
        } else {
            false
        };
        if let Ok(Some(cfg)) = self.load_workflow_config() {
            let routing = workflow_models::parse_routing_block(&cfg, markdown)
                .unwrap_or_else(|| workflow_models::classify(&cfg, markdown));
            self.workflow.routing = Some(routing);
        }
        self.persist_plan();
    }

    pub(super) fn persist_plan(&mut self) {
        let Some(thread_id) = self.thread_id else {
            return;
        };
        let Some(markdown) = self.workflow.plan_markdown.clone() else {
            return;
        };
        let plan = workflow_models::PersistedPlan {
            markdown,
            workflow: self.workflow.workflow.clone(),
            routing: self.workflow.routing.clone(),
            approved: self.workflow.plan_approved,
            auto_enabled: self.workflow.auto_enabled,
        };
        if let Err(err) =
            workflow_models::save_plan(&self.config.codex_home, &thread_id.to_string(), &plan)
        {
            self.add_error_message(format!("Failed to persist plan: {err}"));
        }
    }

    /// Count a genuine failed implementation cycle and recommend escalation.
    pub(super) fn note_failed_attempt(&mut self) {
        self.workflow.failed_attempts = self.workflow.failed_attempts.saturating_add(1);
        if self.workflow.failed_attempts < 2 {
            return;
        }
        let model_id = self.current_model().to_string();
        let on_deepseek = self
            .load_workflow_config()
            .ok()
            .flatten()
            .and_then(|cfg| {
                workflow_models::symbolic_for_model_id(&cfg, &model_id)
                    .map(|(symbolic, _)| symbolic == "deepseek")
            })
            .unwrap_or(false);
        if on_deepseek {
            self.add_info_message(
                "Two failed correction cycles: consider escalating with /complex (GLM High) or /debug (GLM Max).".to_string(),
                /*hint*/ None,
            );
        }
    }
}

/// Find the built-in preset whose approval policy and permission profile match a
/// captured raw permission state. Used to restore sessions that have no named
/// active permission profile (for example YOLO / danger-full-access).
fn preset_matching(
    approval: &AskForApproval,
    profile: &PermissionProfile,
) -> Option<ApprovalPreset> {
    builtin_approval_presets().into_iter().find(|preset| {
        AskForApproval::from(preset.approval) == *approval && preset.permission_profile == *profile
    })
}

/// Next advertised reasoning level in the requested direction.
fn step_effort(
    choices: &[ReasoningEffortConfig],
    current: Option<&ReasoningEffortConfig>,
    raise: bool,
) -> Option<ReasoningEffortConfig> {
    let current = current?;
    let index = choices.iter().position(|choice| choice == current)?;
    if raise {
        choices.get(index + 1).cloned()
    } else {
        index.checked_sub(1).and_then(|index| choices.get(index)).cloned()
    }
}

fn reasoning_rank(level: &str) -> i32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::protocol::AskForApproval as ProtocolAskForApproval;

    fn approval(policy: ProtocolAskForApproval) -> AskForApproval {
        AskForApproval::from(policy)
    }

    #[test]
    fn preset_matching_recognises_unrestricted_sessions() {
        // YOLO (`--dangerously-bypass-approvals-and-sandbox`) has no named
        // profile: only the raw approval + disabled profile identify it.
        let preset = preset_matching(&approval(ProtocolAskForApproval::Never), &PermissionProfile::Disabled)
            .expect("unrestricted state must match a built-in preset");
        assert_eq!(preset.id, "full-access");
    }

    #[test]
    fn preset_matching_recognises_named_states() {
        assert_eq!(
            preset_matching(
                &approval(ProtocolAskForApproval::OnRequest),
                &PermissionProfile::workspace_write(),
            )
            .map(|preset| preset.id),
            Some("auto"),
        );
        assert_eq!(
            preset_matching(
                &approval(ProtocolAskForApproval::OnRequest),
                &PermissionProfile::read_only(),
            )
            .map(|preset| preset.id),
            Some("read-only"),
        );
    }

    #[test]
    fn preset_matching_rejects_mismatched_state() {
        // Approval and profile must both match; a read-only profile paired with
        // a never-ask policy is not a built-in preset and must not silently
        // resolve to a different one.
        assert!(
            preset_matching(
                &approval(ProtocolAskForApproval::Never),
                &PermissionProfile::read_only(),
            )
            .is_none()
        );
    }
}
