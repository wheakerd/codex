use std::sync::Arc;

use std::future::Future;

use codex_agent_identity::AgentIdentityKey;
use codex_agent_identity::agent_identity_authapi_base_url_from_chatgpt_base_url;
use codex_agent_identity::is_retryable_registration_error;
use codex_agent_identity::register_agent_task;
use codex_protocol::account::PlanType as AccountPlanType;
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::default_client::build_reqwest_client;

use super::storage::AgentIdentityAuthRecord;

const DEFAULT_CHATGPT_BACKEND_BASE_URL: &str = "https://chatgpt.com/backend-api";
pub(super) const MAX_AGENT_IDENTITY_BOOTSTRAP_ATTEMPTS: usize = 3;

#[derive(Debug, Error)]
#[error("retryable agent identity registration failure: {message}")]
pub(super) struct RetryableAgentIdentityRegistrationError {
    message: String,
}

impl RetryableAgentIdentityRegistrationError {
    pub(super) fn new(message: String) -> Self {
        Self { message }
    }
}

pub struct AgentIdentityAuth {
    record: AgentIdentityAuthRecord,
    run_task_id: Arc<OnceCell<String>>,
}

impl Clone for AgentIdentityAuth {
    fn clone(&self) -> Self {
        Self {
            record: self.record.clone(),
            run_task_id: Arc::clone(&self.run_task_id),
        }
    }
}

impl std::fmt::Debug for AgentIdentityAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentIdentityAuth")
            .field("record", &self.record)
            .field("run_task_id", &self.run_task_id.get())
            .finish()
    }
}

impl AgentIdentityAuth {
    pub fn new(record: AgentIdentityAuthRecord) -> Self {
        let run_task_id = Arc::new(OnceCell::new());
        if let Some(task_id) = record.task_id.as_ref() {
            let _ = run_task_id.set(task_id.clone());
        }
        Self {
            record,
            run_task_id,
        }
    }

    pub fn record(&self) -> &AgentIdentityAuthRecord {
        &self.record
    }

    pub fn run_task_id(&self) -> Option<String> {
        self.run_task_id.get().cloned()
    }

    pub async fn ensure_run_task(&self, chatgpt_base_url: Option<String>) -> std::io::Result<()> {
        self.run_task_id_for(chatgpt_base_url).await.map(|_| ())
    }

    pub async fn register_task(&self, chatgpt_base_url: Option<String>) -> std::io::Result<String> {
        register_task_for_record(&self.record, chatgpt_base_url).await
    }

    pub fn account_id(&self) -> &str {
        &self.record.account_id
    }

    pub fn chatgpt_user_id(&self) -> &str {
        &self.record.chatgpt_user_id
    }

    pub fn email(&self) -> &str {
        &self.record.email
    }

    pub fn plan_type(&self) -> AccountPlanType {
        self.record.plan_type
    }

    pub fn is_fedramp_account(&self) -> bool {
        self.record.chatgpt_account_is_fedramp
    }
    async fn run_task_id_for(&self, chatgpt_base_url: Option<String>) -> std::io::Result<String> {
        self.run_task_id
            .get_or_try_init(|| async {
                let task_id = self.register_task(chatgpt_base_url).await?;
                Ok(task_id)
            })
            .await
            .cloned()
    }
}

pub(super) async fn record_with_run_task(
    mut record: AgentIdentityAuthRecord,
    chatgpt_base_url: Option<String>,
) -> std::io::Result<AgentIdentityAuthRecord> {
    if record.task_id.is_none() {
        record.task_id =
            Some(register_task_for_record_with_retries(&record, chatgpt_base_url).await?);
    }
    Ok(record)
}

pub(super) fn is_retryable_io_registration_error(err: &std::io::Error) -> bool {
    err.get_ref().is_some_and(
        <dyn std::error::Error + std::marker::Send + std::marker::Sync + 'static>::is::<
            RetryableAgentIdentityRegistrationError,
        >,
    )
}

pub(super) async fn retry_registration<T, F, Fut>(mut operation: F) -> std::io::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    let mut attempt = 1;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(err)
                if attempt < MAX_AGENT_IDENTITY_BOOTSTRAP_ATTEMPTS
                    && is_retryable_io_registration_error(&err) =>
            {
                tracing::warn!(
                    attempt,
                    max_attempts = MAX_AGENT_IDENTITY_BOOTSTRAP_ATTEMPTS,
                    error = %err,
                    "agent identity registration attempt failed; retrying"
                );
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn register_task_for_record_with_retries(
    record: &AgentIdentityAuthRecord,
    chatgpt_base_url: Option<String>,
) -> std::io::Result<String> {
    retry_registration(async || register_task_for_record(record, chatgpt_base_url.clone()).await)
        .await
}

async fn register_task_for_record(
    record: &AgentIdentityAuthRecord,
    chatgpt_base_url: Option<String>,
) -> std::io::Result<String> {
    let authapi_base_url = agent_identity_authapi_base_url_from_chatgpt_base_url(
        chatgpt_base_url
            .as_deref()
            .unwrap_or(DEFAULT_CHATGPT_BACKEND_BASE_URL),
    );
    register_agent_task(
        &build_reqwest_client(),
        &authapi_base_url,
        key_for_record(record),
    )
    .await
    .map_err(|err| {
        if is_retryable_registration_error(&err) {
            std::io::Error::other(RetryableAgentIdentityRegistrationError::new(
                err.to_string(),
            ))
        } else {
            std::io::Error::other(err)
        }
    })
}

fn key_for_record(record: &AgentIdentityAuthRecord) -> AgentIdentityKey<'_> {
    AgentIdentityKey {
        agent_runtime_id: &record.agent_runtime_id,
        private_key_pkcs8_base64: &record.agent_private_key,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use codex_agent_identity::generate_agent_key_material;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    fn agent_identity_record(private_key: String) -> AgentIdentityAuthRecord {
        AgentIdentityAuthRecord {
            agent_runtime_id: "agent-runtime-1".to_string(),
            agent_private_key: private_key,
            account_id: "account-1".to_string(),
            chatgpt_user_id: "user-1".to_string(),
            email: "agent@example.com".to_string(),
            plan_type: AccountPlanType::Plus,
            chatgpt_account_is_fedramp: false,
            task_id: None,
        }
    }

    fn agent_identity_auth() -> AgentIdentityAuth {
        let key_material = generate_agent_key_material().expect("generate key material");
        AgentIdentityAuth::new(agent_identity_record(key_material.private_key_pkcs8_base64))
    }

    #[tokio::test]
    async fn ensure_run_task_registers_once() -> anyhow::Result<()> {
        let auth = agent_identity_auth();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/agent/agent-runtime-1/task/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "task_id": "task-run-1",
            })))
            .expect(1)
            .mount(&server)
            .await;

        auth.ensure_run_task(Some(server.uri())).await?;
        auth.ensure_run_task(Some(server.uri())).await?;

        assert_eq!(auth.run_task_id(), Some("task-run-1".to_string()));
        let requests = server
            .received_requests()
            .await
            .expect("failed to fetch task registration request");
        let request_body = requests[0]
            .body_json::<serde_json::Value>()
            .expect("task registration request should be JSON");
        let request_body = request_body
            .as_object()
            .expect("request body should be object");
        assert!(request_body.get("timestamp").is_some());
        assert!(request_body.get("signature").is_some());
        assert_eq!(request_body.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn run_task_is_shared_across_clones() -> anyhow::Result<()> {
        let auth = agent_identity_auth();
        let cloned = auth.clone();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/agent/agent-runtime-1/task/register"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "task_id": "task-run-1",
            })))
            .expect(1)
            .mount(&server)
            .await;

        auth.ensure_run_task(Some(server.uri())).await?;
        cloned.ensure_run_task(Some(server.uri())).await?;

        assert_eq!(cloned.run_task_id(), Some("task-run-1".to_string()));
        Ok(())
    }

    #[tokio::test]
    async fn failed_run_task_registration_can_retry() -> anyhow::Result<()> {
        let auth = agent_identity_auth();
        let server = MockServer::start().await;
        let request_count = Arc::new(AtomicUsize::new(0));
        let response_count = Arc::clone(&request_count);
        Mock::given(method("POST"))
            .and(path("/v1/agent/agent-runtime-1/task/register"))
            .respond_with(move |_request: &wiremock::Request| {
                if response_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(500)
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "task_id": "task-run-1",
                    }))
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        auth.ensure_run_task(Some(server.uri()))
            .await
            .expect_err("first registration should fail");
        auth.ensure_run_task(Some(server.uri())).await?;

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert_eq!(auth.run_task_id(), Some("task-run-1".to_string()));
        Ok(())
    }

    #[test]
    fn run_task_id_is_shared_across_clones() {
        let auth = agent_identity_auth();
        let cloned = auth.clone();
        auth.run_task_id
            .set("task-run-1".to_string())
            .expect("run task should be unset");

        assert_eq!(cloned.run_task_id(), Some("task-run-1".to_string()));
    }
}
