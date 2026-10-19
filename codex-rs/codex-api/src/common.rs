use crate::error::ApiError;
use codex_protocol::ResponseUsageMetadata;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::config_types::Verbosity as VerbosityConfig;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::protocol::ModelVerification;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnModerationMetadataEvent;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::turn_input::CyberAccessProgram;
use futures::Stream;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

pub const WS_REQUEST_HEADER_TRACEPARENT_CLIENT_METADATA_KEY: &str = "ws_request_header_traceparent";
pub const WS_REQUEST_HEADER_TRACESTATE_CLIENT_METADATA_KEY: &str = "ws_request_header_tracestate";

/// Explicit per-request access selection using the Responses API wire values.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AccessPrograms {
    cyber: &'static str,
}

impl From<CyberAccessProgram> for AccessPrograms {
    fn from(program: CyberAccessProgram) -> Self {
        Self {
            cyber: match program {
                CyberAccessProgram::Standard => "standard",
                CyberAccessProgram::DaybreakBlue => "daybreak_blue",
                CyberAccessProgram::DaybreakRed => "daybreak_red",
            },
        }
    }
}

/// Canonical input payload for the memory summarize endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct MemorySummarizeInput {
    pub model: String,
    #[serde(rename = "traces")]
    pub raw_memories: Vec<RawMemory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Reasoning>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RawMemory {
    pub id: String,
    pub metadata: RawMemoryMetadata,
    pub items: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RawMemoryMetadata {
    pub source_path: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct MemorySummarizeOutput {
    #[serde(rename = "trace_summary", alias = "raw_memory")]
    pub raw_memory: String,
    pub memory_summary: String,
}

/// The latest server response ID received in this turn, shared with tool-review extensions.
#[derive(Clone, Debug)]
pub struct ResponseId(pub String);

#[derive(Debug)]
pub enum ResponseEvent {
    Created {
        /// Existing server response ID, when supplied by the stream.
        response_id: Option<String>,
    },
    SafetyBuffering(SafetyBuffering),
    OutputItemDone(ResponseItem),
    OutputItemAdded(ResponseItem),
    /// Emitted when the server includes `OpenAI-Model` on the stream response.
    /// This can differ from the requested model when backend safety routing applies.
    ServerModel(String),
    /// Emitted when the server recommends additional account verification.
    ModelVerifications(Vec<ModelVerification>),
    /// Emitted when the server includes moderation metadata for first-party turn presentation.
    TurnModerationMetadata(TurnModerationMetadataEvent),
    /// Emitted when `X-Reasoning-Included: true` is present on the response,
    /// meaning the server already accounted for past reasoning tokens and the
    /// client should not re-estimate them.
    ServerReasoningIncluded(bool),
    Completed {
        response_id: String,
        token_usage: Option<TokenUsage>,
        usage_metadata: Option<ResponseUsageMetadata>,
        /// Did the model affirmatively end its turn? Some providers do not set this,
        /// so we rely on fallback logic when this is `None`.
        end_turn: Option<bool>,
    },
    OutputTextDelta(String),
    ToolCallInputDelta {
        item_id: String,
        call_id: Option<String>,
        delta: String,
    },
    ReasoningSummaryDelta {
        delta: String,
        summary_index: i64,
    },
    ReasoningSummaryDone {
        item_id: String,
        text: String,
        summary_index: i64,
    },
    ReasoningContentDelta {
        delta: String,
        content_index: i64,
    },
    ReasoningSummaryPartAdded {
        summary_index: i64,
    },
    RateLimits(RateLimitSnapshot),
    ModelsEtag(String),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SafetyBuffering {
    pub use_cases: Vec<String>,
    pub reasons: Vec<String>,
    #[serde(skip)]
    pub show_buffering_ui: bool,
    #[serde(rename = "retry_model")]
    pub faster_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SafetyBufferingTreatment {
    pub faster_model: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningContext {
    Auto,
    CurrentTurn,
    AllTurns,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Reasoning {
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_reasoning_effort"
    )]
    pub effort: Option<ReasoningEffortConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummaryConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ReasoningContext>,
}

fn serialize_reasoning_effort<S>(
    effort: &Option<ReasoningEffortConfig>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if let Some(ReasoningEffortConfig::Custom(value)) = effort
        && let Ok(value) = value.parse::<u64>()
    {
        return serializer.serialize_u64(value);
    }
    effort.serialize(serializer)
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSummaryDelivery {
    SequentialCutoff,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct StreamOptions {
    pub reasoning_summary_delivery: ReasoningSummaryDelivery,
}

#[derive(Debug, Serialize, Default, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TextFormatType {
    #[default]
    JsonSchema,
}

#[derive(Debug, Serialize, Default, Clone, PartialEq)]
pub struct TextFormat {
    /// Format type used by the OpenAI text controls.
    pub r#type: TextFormatType,
    /// When true, the server is expected to strictly validate responses.
    pub strict: bool,
    /// JSON schema for the desired output.
    pub schema: Value,
    /// Friendly name for the format, used in telemetry/debugging.
    pub name: String,
}

/// Controls the `text` field for the Responses API, combining verbosity and
/// optional JSON schema output formatting.
#[derive(Debug, Serialize, Default, Clone, PartialEq)]
pub struct TextControls {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<OpenAiVerbosity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<TextFormat>,
}

#[derive(Debug, Serialize, Default, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OpenAiVerbosity {
    Low,
    #[default]
    Medium,
    High,
}

impl From<VerbosityConfig> for OpenAiVerbosity {
    fn from(v: VerbosityConfig) -> Self {
        match v {
            VerbosityConfig::Low => OpenAiVerbosity::Low,
            VerbosityConfig::Medium => OpenAiVerbosity::Medium,
            VerbosityConfig::High => OpenAiVerbosity::High,
        }
    }
}

/// Serialized tool definitions for Responses API requests.
///
/// Keeping the tool list as raw JSON avoids rebuilding a generic JSON value
/// tree, while the shared allocation keeps request clones cheap.
#[derive(Debug, Clone)]
pub struct ResponsesApiTools(Arc<RawValue>);

impl ResponsesApiTools {
    pub(crate) fn as_raw_value(&self) -> &RawValue {
        &self.0
    }
}

impl From<Arc<RawValue>> for ResponsesApiTools {
    fn from(value: Arc<RawValue>) -> Self {
        Self(value)
    }
}

impl PartialEq for ResponsesApiTools {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.get() == other.0.get()
    }
}

impl Serialize for ResponsesApiTools {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ResponsesApiRequest {
    // Keep routing fields first: serde serializes struct fields in declaration order, and
    // gateways may inspect request bodies incrementally before potentially multi-megabyte input.
    pub model: String,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub input: Vec<ResponseItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ResponsesApiTools>,
    pub tool_choice: String,
    pub parallel_tool_calls: bool,
    pub reasoning: Option<Reasoning>,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    pub include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<TextControls>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_metadata: Option<HashMap<String, String>>,
    /// Optional sticky-routing key surfaced to gateways that support it
    /// (e.g. OpenRouter's `session_id`). Omitted from the body when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Extra top-level JSON fields merged into the request body for this
    /// provider (e.g. OpenRouter `provider` routing preferences). Empty by
    /// default so it serializes to nothing.
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_body: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_programs: Option<AccessPrograms>,
}

impl<'a> From<&'a ResponsesApiRequest> for ResponseCreateWsRequest<'a> {
    fn from(request: &'a ResponsesApiRequest) -> Self {
        Self {
            model: &request.model,
            previous_response_id: None,
            input: &request.input,
            tools: request.tools.as_ref().map(ResponsesApiTools::as_raw_value),
            tool_choice: &request.tool_choice,
            parallel_tool_calls: request.parallel_tool_calls,
            reasoning: request.reasoning.as_ref(),
            store: request.store,
            stream: request.stream,
            stream_options: request.stream_options.as_ref(),
            include: &request.include,
            service_tier: request.service_tier.as_deref(),
            prompt_cache_key: request.prompt_cache_key.as_deref(),
            text: request.text.as_ref(),
            generate: None,
            client_metadata: request.client_metadata.clone(),
            session_id: request.session_id.as_deref(),
            extra_body: &request.extra_body,
            access_programs: request.access_programs,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ResponseCreateWsRequest<'a> {
    // Keep routing fields first: serde serializes struct fields in declaration order, and
    // gateways may inspect request bodies incrementally before potentially multi-megabyte input.
    pub model: &'a str,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    pub input: &'a [ResponseItem],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a RawValue>,
    pub tool_choice: &'a str,
    pub parallel_tool_calls: bool,
    pub reasoning: Option<&'a Reasoning>,
    pub store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<&'a StreamOptions>,
    pub include: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<&'a TextControls>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generate: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_metadata: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<&'a str>,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_body: &'a BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_programs: Option<AccessPrograms>,
}

pub fn response_create_client_metadata(
    client_metadata: Option<HashMap<String, String>>,
    trace: Option<&W3cTraceContext>,
) -> Option<HashMap<String, String>> {
    let mut client_metadata = client_metadata.unwrap_or_default();

    if let Some(traceparent) = trace.and_then(|trace| trace.traceparent.as_deref()) {
        client_metadata.insert(
            WS_REQUEST_HEADER_TRACEPARENT_CLIENT_METADATA_KEY.to_string(),
            traceparent.to_string(),
        );
    }
    if let Some(tracestate) = trace.and_then(|trace| trace.tracestate.as_deref()) {
        client_metadata.insert(
            WS_REQUEST_HEADER_TRACESTATE_CLIENT_METADATA_KEY.to_string(),
            tracestate.to_string(),
        );
    }

    (!client_metadata.is_empty()).then_some(client_metadata)
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum ResponsesWsRequest<'a> {
    #[serde(rename = "response.create")]
    ResponseCreate(ResponseCreateWsRequest<'a>),
}

pub fn create_text_param_for_request(
    verbosity: Option<VerbosityConfig>,
    output_schema: &Option<Value>,
    output_schema_strict: bool,
) -> Option<TextControls> {
    if verbosity.is_none() && output_schema.is_none() {
        return None;
    }

    Some(TextControls {
        verbosity: verbosity.map(std::convert::Into::into),
        format: output_schema.as_ref().map(|schema| TextFormat {
            r#type: TextFormatType::JsonSchema,
            strict: output_schema_strict,
            schema: schema.clone(),
            name: "codex_output_schema".to_string(),
        }),
    })
}

pub struct ResponseStream {
    pub rx_event: mpsc::Receiver<Result<ResponseEvent, ApiError>>,
    /// Server-assigned `x-request-id` response header, when present.
    pub upstream_request_id: Option<String>,
    /// Requests a graceful interrupt. Keep consuming events through completion.
    pub interrupt: Option<oneshot::Sender<()>>,
}

impl Stream for ResponseStream {
    type Item = Result<ResponseEvent, ApiError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx_event.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> ResponsesApiRequest {
        ResponsesApiRequest {
            model: "test-model".to_string(),
            stream: true,
            service_tier: None,
            input: Vec::new(),
            tools: None,
            tool_choice: "auto".to_string(),
            parallel_tool_calls: true,
            reasoning: None,
            store: false,
            stream_options: None,
            include: Vec::new(),
            prompt_cache_key: None,
            text: None,
            client_metadata: None,
            session_id: None,
            extra_body: BTreeMap::new(),
            access_programs: None,
        }
    }

    #[test]
    fn session_id_and_extra_body_serialize_at_top_level() {
        let mut request = base_request();
        request.session_id = Some("thread-123".to_string());
        request.extra_body.insert(
            "provider".to_string(),
            serde_json::json!({
                "allow_fallbacks": true,
                "order": ["DeepInfra/turbo"],
            }),
        );

        let value = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(value["session_id"], serde_json::json!("thread-123"));
        assert_eq!(
            value["provider"]["allow_fallbacks"],
            serde_json::json!(true)
        );
        assert_eq!(
            value["provider"]["order"][0],
            serde_json::json!("DeepInfra/turbo")
        );
    }

    #[test]
    fn empty_session_id_and_extra_body_are_omitted() {
        let value = serde_json::to_value(base_request()).expect("serialize request");
        assert!(value.get("session_id").is_none());
        assert!(value.get("provider").is_none());
    }

    #[test]
    fn websocket_request_carries_session_id_and_extra_body() {
        let mut request = base_request();
        request.session_id = Some("thread-123".to_string());
        request.extra_body.insert(
            "provider".to_string(),
            serde_json::json!({ "allow_fallbacks": false }),
        );

        let ws_request = ResponseCreateWsRequest::from(&request);
        let value = serde_json::to_value(&ws_request).expect("serialize ws request");
        assert_eq!(value["session_id"], serde_json::json!("thread-123"));
        assert_eq!(
            value["provider"]["allow_fallbacks"],
            serde_json::json!(false)
        );
    }
}
