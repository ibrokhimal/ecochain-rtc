//! Configurable HTTP providers. Credentials remain on the server.
pub mod captions;
pub mod jira;
pub mod jobs;
pub mod speech;
use crate::config::Secret;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Optional backend integrations; absence disables their capabilities explicitly.
#[derive(Debug, Clone, Default)]
pub struct ProvidersConfig {
    /// OpenAI-compatible audio transcription endpoint.
    pub stt: Option<SttConfig>,
    /// OpenAI-compatible chat-completion endpoint for editable draft summaries.
    pub summary: Option<SummaryConfig>,
    /// Jira Cloud API integration.
    pub jira: Option<JiraConfig>,
}
/// Audio transcription adapter settings, resolved at startup.
#[derive(Debug, Clone)]
pub struct SttConfig {
    /// Full /audio/transcriptions endpoint.
    pub endpoint: String,
    /// Bearer credential from environment.
    pub api_key: Secret,
    /// Provider-supported transcription model, explicitly configured.
    pub model: String,
    /// json or verbose_json. Plain json uses the known chunk time interval.
    pub response_format: String,
    /// Optional ISO language; absent requests auto detection.
    pub language: Option<String>,
}
/// Draft-only summary adapter settings.
#[derive(Debug, Clone)]
pub struct SummaryConfig {
    /// Full /chat/completions endpoint.
    pub endpoint: String,
    /// Bearer credential from environment.
    pub api_key: Secret,
    /// Explicit provider model.
    pub model: String,
}
/// Jira Cloud target. Request bodies never choose the credential destination.
#[derive(Debug, Clone)]
pub struct JiraConfig {
    /// HTTPS origin of the configured Jira instance.
    pub base_url: String,
    /// Jira API user's email.
    pub email: String,
    /// API token from environment.
    pub api_token: Secret,
    /// Explicit project allowlist.
    pub project_keys: Vec<String>,
    /// Project-compatible issue type ID.
    pub issue_type_id: String,
    /// User-facing type label, normally Task.
    pub issue_type_name: String,
    /// Optional mapping of meeting identities to Jira account IDs.
    pub identity_account_ids: BTreeMap<String, String>,
}
/// A timestamped utterance with a known source identity or an explicit unknown source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    /// Milliseconds relative to transcript/recording start.
    pub start_ms: u64,
    /// Exclusive end of the utterance.
    pub end_ms: u64,
    /// Source participant ID, empty if unknown.
    pub participant_id: String,
    /// Speaker display label.
    pub name: String,
    /// Provider text, always treated as untrusted content.
    pub text: String,
}
/// A known meeting identity offered to the summary model for draft assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParticipantLabel {
    /// Backend identity, not a model-invented name.
    pub identity: String,
    /// Display label.
    pub name: String,
}
/// A model-proposed task, never automatically submitted or made official.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftTask {
    /// Task body awaiting review.
    pub text: String,
    /// Known meeting identity, or None.
    pub assignee: Option<String>,
    /// Calendar date YYYY-MM-DD, or None.
    pub due_date: Option<String>,
}
/// Editable draft output; confirmation is a separate server action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryDraft {
    /// Concise meeting summary.
    pub summary: String,
    /// Proposed decisions awaiting review.
    pub decisions: Vec<String>,
    /// Proposed tasks awaiting review.
    pub tasks: Vec<DraftTask>,
}
/// Builds a bounded provider client with redirects disabled to protect credentials.
pub fn client() -> Result<reqwest::Client> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("ecochain-rtc/0.2.0")
        .build()?)
}
/// Parses a bounded JSON body; callers decide how to classify HTTP status codes.
pub async fn bounded_json(mut response: reqwest::Response, max: usize) -> Result<Value> {
    if let Some(length) = response.content_length() {
        ensure!(length <= max as u64, "provider response too large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= max,
            "provider response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
/// Validates an ISO calendar date without accepting impossible month/day combinations.
pub fn valid_date(date: &str) -> bool {
    let b = date.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || b.iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = date[..4].parse().unwrap();
    let month: usize = date[5..7].parse().unwrap();
    let day: u32 = date[8..].parse().unwrap();
    if year == 0 || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    day > 0 && day <= days[month - 1]
}

/// Explicitly reviewed draft application; it is separate from model generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryConfirmation {
    /// Summary job whose draft the host reviewed.
    pub job_id: String,
    /// Workspace version shown during review.
    pub expected_revision: u64,
    /// User-edited summary, decisions and tasks.
    pub draft: SummaryDraft,
}
/// Builds one atomic, idempotent workspace update from a host-confirmed draft.
pub(crate) fn confirmed_snapshot(
    state: &Value,
    members: &std::collections::BTreeSet<String>,
    author: &str,
    confirmation: &SummaryConfirmation,
) -> Result<Value, crate::protocol::ProtocolError> {
    use crate::protocol::{ErrorCode, ProtocolError};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let bad = |message: &str| ProtocolError::new(ErrorCode::BadRequest, message);
    let conflict = || {
        ProtocolError::new(
            ErrorCode::InvalidState,
            "workspace changed or summary was confirmed with different content",
        )
    };
    if uuid::Uuid::parse_str(&confirmation.job_id).is_err() {
        return Err(bad("invalid summary job id"));
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&confirmation.draft).map_err(|_| bad("invalid draft"))?)
    );
    if let Some(previous) = state
        .get("confirmed_summaries")
        .and_then(|v| v.get(&confirmation.job_id))
    {
        return if previous == &hash {
            Ok(state.clone())
        } else {
            Err(conflict())
        };
    }
    if state["revision"].as_u64() != Some(confirmation.expected_revision) {
        return Err(conflict());
    }
    let draft = &confirmation.draft;
    if draft.summary.len() > 16384 || draft.decisions.len() > 100 || draft.tasks.len() > 100 {
        return Err(bad("draft exceeds limits"));
    }
    if draft
        .decisions
        .iter()
        .any(|s| s.trim().is_empty() || s.len() > 8000)
        || draft.tasks.iter().any(|t| {
            t.text.trim().is_empty()
                || t.text.len() > 8000
                || t.assignee.as_ref().is_some_and(|id| !members.contains(id))
                || t.due_date.as_ref().is_some_and(|date| !valid_date(date))
        })
    {
        return Err(bad("invalid draft decision, task, assignee or date"));
    }
    let mut next = state.clone();
    for (key, count) in [
        ("notes", usize::from(!draft.summary.trim().is_empty())),
        ("decisions", draft.decisions.len()),
        ("tasks", draft.tasks.len()),
    ] {
        if next[key].as_array().map_or(0, Vec::len) + count > 1000 {
            return Err(ProtocolError::new(
                ErrorCode::LimitExceeded,
                "too many workspace items",
            ));
        }
    }
    let mut additions: Vec<(&str, Value)> = Vec::new();
    if !draft.summary.trim().is_empty() {
        additions.push(("notes",json!({"id":format!("summary_{}",confirmation.job_id),"text":draft.summary,"author_id":author,"revision":1})));
    }
    for (i, text) in draft.decisions.iter().enumerate() {
        additions.push(("decisions",json!({"id":format!("decision_{}_{i}",confirmation.job_id),"text":text,"author_id":author,"revision":1})));
    }
    for (i, task) in draft.tasks.iter().enumerate() {
        additions.push(("tasks",json!({"id":format!("task_{}_{i}",confirmation.job_id),"text":task.text,"author_id":author,"revision":1,"assignee":task.assignee.as_deref().unwrap_or(""),"due_date":task.due_date.as_deref().unwrap_or(""),"status":"open"})));
    }
    for (key, item) in additions {
        if !next[key].is_array() {
            next[key] = json!([]);
        }
        let list = next[key].as_array_mut().unwrap();
        if list.iter().any(|old| old["id"] == item["id"]) {
            return Err(conflict());
        }
        list.push(item);
    }
    if !next["confirmed_summaries"].is_object() {
        next["confirmed_summaries"] = json!({});
    }
    if next["confirmed_summaries"].as_object().unwrap().len() >= 1000 {
        return Err(ProtocolError::new(
            ErrorCode::LimitExceeded,
            "too many confirmed summaries",
        ));
    }
    next["confirmed_summaries"][&confirmation.job_id] = json!(hash);
    next["revision"] = json!(confirmation.expected_revision + 1);
    if next.to_string().len() > 10 * 1024 * 1024 {
        return Err(ProtocolError::new(
            ErrorCode::LimitExceeded,
            "workspace exceeds 10 MiB",
        ));
    }
    Ok(next)
}
