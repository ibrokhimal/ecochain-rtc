//! Jira HTTP adapter.

use super::{bounded_json, valid_date, JiraConfig};
use anyhow::{anyhow, bail, ensure, Result};
use reqwest::{header::ACCEPT, Client, Method, RequestBuilder, Response, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use thiserror::Error;

const PROPERTY_KEY: &str = "ecochain_meeting";
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_PAGES: usize = 10;
const PROJECT_PAGE_SIZE: usize = 50;
const ISSUE_PAGE_SIZE: usize = 10;

/// User-reviewed issue fields bound to a specific task and allowed project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraPreview {
    pub task_id: String,
    pub project_key: String,
    pub summary: String,
    pub description: String,
    pub assignee_identity: Option<String>,
    pub due_date: Option<String>,
}

/// An allowed project visible to the configured Jira account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraProject {
    pub key: String,
    pub name: String,
}

/// A created or verified existing Jira issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraIssue {
    pub id: String,
    pub key: String,
    pub url: String,
}

/// Whether the caller can safely treat the issue creation as rejected.
#[derive(Debug, Error)]
pub enum JiraCreateError {
    #[error("{0}")]
    Definitive(String),
    #[error("{0}")]
    Uncertain(String),
}

/// Stable SHA-256 reference for the task in this meeting instance.
pub fn operation_key(instance: &str, task_id: &str) -> String {
    let mut hasher = Sha256::new();
    // Prefixing the length keeps ("ab", "c") and ("a", "bc") distinct.
    hasher.update((instance.len() as u64).to_be_bytes());
    hasher.update(instance.as_bytes());
    hasher.update(task_id.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Validates the approved fields and translates them to Jira Cloud's create format.
pub fn build_payload(config: &JiraConfig, instance: &str, preview: &JiraPreview) -> Result<Value> {
    check_project(config, &preview.project_key)?;
    ensure!(
        !config.issue_type_id.trim().is_empty(),
        "Jira issue type is missing"
    );
    ensure!(
        !instance.is_empty() && !preview.task_id.is_empty(),
        "Jira task reference is missing"
    );
    ensure!(
        !preview.summary.trim().is_empty() && preview.summary.chars().count() <= 255,
        "Jira summary must contain 1 to 255 characters",
    );

    let paragraphs: Vec<_> = preview
        .description
        .split('\n')
        .map(|line| {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                json!({"type": "paragraph"})
            } else {
                json!({"type": "paragraph", "content": [{"type": "text", "text": line}]})
            }
        })
        .collect();
    let assignee = preview
        .assignee_identity
        .as_ref()
        .and_then(|identity| config.identity_account_ids.get(identity))
        .map(|account_id| json!({"accountId": account_id}));
    let key = operation_key(instance, &preview.task_id);
    let mut fields = json!({
        "project": {"key": preview.project_key},
        "issuetype": {"id": config.issue_type_id},
        "summary": preview.summary,
        "description": {"type": "doc", "version": 1, "content": paragraphs},
        "assignee": assignee,
        "labels": [format!("ecochain-{key}")],
    });
    if let Some(due_date) = &preview.due_date {
        ensure!(
            valid_date(due_date),
            "Jira due date must be a valid YYYY-MM-DD date"
        );
        fields["duedate"] = json!(due_date);
    }
    Ok(json!({
        "fields": fields,
        "properties": [{
            "key": PROPERTY_KEY,
            "value": meeting_reference(instance, &preview.task_id, &key)
        }]
    }))
}

/// Lists visible projects on the configured allowlist.
pub async fn projects(client: &Client, config: &JiraConfig) -> Result<Vec<JiraProject>> {
    let origin = origin(config)?;
    let mut projects = BTreeMap::new();
    let mut start_at = 0;
    for _ in 0..MAX_PAGES {
        let mut query = vec![
            ("startAt", start_at.to_string()),
            ("maxResults", PROJECT_PAGE_SIZE.to_string()),
        ];
        query.extend(config.project_keys.iter().cloned().map(|key| ("keys", key)));
        let response = request(
            client,
            config,
            Method::GET,
            &origin,
            "/rest/api/3/project/search",
        )
        .query(&query);
        let page: ProjectPage =
            read_success(send(response, "project search").await?, "project search").await?;
        ensure!(
            page.start_at == start_at && page.values.len() <= PROJECT_PAGE_SIZE,
            "Jira project search returned invalid pagination",
        );
        start_at += page.values.len();
        ensure!(
            page.is_last || !page.values.is_empty(),
            "Jira project search did not advance"
        );
        for project in page.values {
            if check_project(config, &project.key).is_ok() {
                projects.insert(project.key.clone(), project);
            }
        }
        if page.is_last {
            return Ok(projects.into_values().collect());
        }
    }
    bail!("Jira project search exceeded the page limit")
}

/// Sends one issue create request. Callers reconcile uncertain outcomes before further action.
pub async fn create_issue(
    client: &Client,
    config: &JiraConfig,
    payload: &Value,
) -> Result<JiraIssue, JiraCreateError> {
    let origin = origin(config).map_err(|error| JiraCreateError::Definitive(error.to_string()))?;
    let project_key = payload["fields"]["project"]["key"]
        .as_str()
        .unwrap_or_default();
    check_project(config, project_key)
        .map_err(|error| JiraCreateError::Definitive(error.to_string()))?;
    let response = send(
        request(client, config, Method::POST, &origin, "/rest/api/3/issue").json(payload),
        "issue creation",
    )
    .await
    .map_err(|error| JiraCreateError::Uncertain(error.to_string()))?;

    let status = response.status();
    if status.is_client_error() {
        return Err(JiraCreateError::Definitive(format!(
            "Jira rejected issue creation (HTTP {})",
            status.as_u16()
        )));
    }
    if status != StatusCode::CREATED {
        return Err(JiraCreateError::Uncertain(format!(
            "Jira issue creation returned HTTP {}",
            status.as_u16()
        )));
    }
    let remote: RemoteIssue = decode(response, "issue creation")
        .await
        .map_err(|error| JiraCreateError::Uncertain(error.to_string()))?;
    remote
        .into_issue(&origin, project_key)
        .map_err(|error| JiraCreateError::Uncertain(error.to_string()))
}

/// Finds a uniquely verified issue for the task by its exact reference label and property.
pub async fn find_existing(
    client: &Client,
    config: &JiraConfig,
    project_key: &str,
    instance: &str,
    task_id: &str,
) -> Result<Option<JiraIssue>> {
    check_project(config, project_key)?;
    let origin = origin(config)?;
    let key = operation_key(instance, task_id);
    let reference = meeting_reference(instance, task_id, &key);
    let mut payload = json!({
        "jql": format!("project = \"{project_key}\" AND labels = \"ecochain-{key}\""),
        "fields": ["key"],
        "maxResults": ISSUE_PAGE_SIZE,
    });
    let mut seen_tokens = HashSet::new();
    let mut found: Option<JiraIssue> = None;
    for _ in 0..MAX_PAGES {
        let search = request(
            client,
            config,
            Method::POST,
            &origin,
            "/rest/api/3/search/jql",
        )
        .json(&payload);
        let page: IssuePage =
            read_success(send(search, "issue search").await?, "issue search").await?;
        ensure!(
            page.issues.len() <= ISSUE_PAGE_SIZE,
            "Jira issue search returned too many results"
        );
        for remote in page.issues {
            let issue = remote.into_issue(&origin, project_key)?;
            let property = request(
                client,
                config,
                Method::GET,
                &origin,
                &format!("/rest/api/3/issue/{}/properties/{PROPERTY_KEY}", issue.key),
            );
            let response = send(property, "issue property lookup").await?;
            if response.status() == StatusCode::NOT_FOUND {
                continue;
            }
            let property: IssueProperty = read_success(response, "issue property lookup").await?;
            if property.key == PROPERTY_KEY && property.value == reference {
                ensure!(
                    found
                        .as_ref()
                        .is_none_or(|previous| previous.id == issue.id),
                    "Multiple Jira issues match the meeting task",
                );
                found = Some(issue);
            }
        }
        if page.is_last {
            return Ok(found);
        }
        let token = page
            .next_page_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| anyhow!("Jira issue search omitted its continuation token"))?;
        ensure!(
            seen_tokens.insert(token.clone()),
            "Jira issue search repeated a continuation token"
        );
        payload["nextPageToken"] = json!(token);
    }
    bail!("Jira issue search exceeded the page limit")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectPage {
    start_at: usize,
    is_last: bool,
    values: Vec<JiraProject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssuePage {
    is_last: bool,
    issues: Vec<RemoteIssue>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct RemoteIssue {
    id: String,
    key: String,
}

impl RemoteIssue {
    fn into_issue(self, origin: &Url, project_key: &str) -> Result<JiraIssue> {
        let numeric = |value: &str| !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit());
        ensure!(
            numeric(&self.id)
                && self
                    .key
                    .split_once('-')
                    .is_some_and(|(project, number)| { project == project_key && numeric(number) }),
            "Jira returned an invalid issue reference",
        );
        let url = url_at(origin, &format!("/browse/{}", self.key));
        Ok(JiraIssue {
            id: self.id,
            key: self.key,
            url: url.into(),
        })
    }
}

#[derive(Deserialize)]
struct IssueProperty {
    key: String,
    value: Value,
}

fn meeting_reference(instance: &str, task_id: &str, key: &str) -> Value {
    json!({"instance_id": instance, "task_id": task_id, "operation_key": key})
}

fn check_project(config: &JiraConfig, project_key: &str) -> Result<()> {
    let mut chars = project_key.bytes();
    ensure!(
        chars.next().is_some_and(|first| first.is_ascii_uppercase())
            && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
            && config
                .project_keys
                .iter()
                .any(|allowed| allowed == project_key),
        "Jira project key is invalid or not allowed",
    );
    Ok(())
}

fn origin(config: &JiraConfig) -> Result<Url> {
    let url = Url::parse(&config.base_url).map_err(|_| anyhow!("Invalid Jira origin"))?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Invalid Jira origin"))?;
    let loopback = matches!(host, "127.0.0.1" | "[::1]" | "localhost");
    ensure!(
        (url.scheme() == "https" || (url.scheme() == "http" && loopback))
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none(),
        "Invalid Jira origin",
    );
    Ok(url)
}

fn url_at(origin: &Url, path: &str) -> Url {
    let mut url = origin.clone();
    url.set_path(path);
    url
}

fn request(
    client: &Client,
    config: &JiraConfig,
    method: Method,
    origin: &Url,
    path: &str,
) -> RequestBuilder {
    client
        .request(method, url_at(origin, path))
        .basic_auth(&config.email, Some(config.api_token.expose()))
        .header(ACCEPT, "application/json")
}

async fn send(request: RequestBuilder, operation: &str) -> Result<Response> {
    // Reqwest and provider errors can include request or response data.
    request
        .send()
        .await
        .map_err(|_| anyhow!("Jira {operation} request failed"))
}

async fn read_success<T: DeserializeOwned>(response: Response, operation: &str) -> Result<T> {
    let status = response.status();
    ensure!(
        status.is_success(),
        "Jira {operation} returned HTTP {}",
        status.as_u16()
    );
    decode(response, operation).await
}

async fn decode<T: DeserializeOwned>(response: Response, operation: &str) -> Result<T> {
    let value = bounded_json(response, MAX_BODY_BYTES)
        .await
        .map_err(|_| anyhow!("Jira {operation} returned an invalid response"))?;
    serde_json::from_value(value)
        .map_err(|_| anyhow!("Jira {operation} returned an invalid response"))
}
