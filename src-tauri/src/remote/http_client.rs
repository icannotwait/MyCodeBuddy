// Thin HTTP client for the daemon's loopback REST surface (capabilities,
// health, conversations). The remote daemon runs on `127.0.0.1:<port>` of
// the desktop host because we forward the port over SSH; the bearer token
// comes from the bootstrap handshake.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::acp::types::{ForkResultInfo, PromptInputBlock};
use crate::acp::LiveSessionSnapshot;
use crate::models::{AgentType, ConversationDetail, ConversationSummary};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CapabilitiesResponse {
    pub version: String,
    #[serde(default)]
    pub schema_version: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub features: CapabilityFlags,
    #[serde(default)]
    pub server_time: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct CapabilityFlags {
    #[serde(default)]
    pub topic_subscribe: bool,
    #[serde(default)]
    pub remote_terminal: bool,
    #[serde(default)]
    pub workspace_watch: bool,
    #[serde(default)]
    pub git_operations: bool,
    #[serde(default)]
    pub file_editing: bool,
}

pub struct DaemonClient {
    base_url: String,
    bearer: String,
    http: reqwest::Client,
}

impl DaemonClient {
    pub fn new(local_port: u16, token: String) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", local_port),
            bearer: token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("build http client"),
        }
    }

    pub async fn capabilities(&self) -> Result<CapabilitiesResponse, ClientError> {
        // Daemon does not yet implement /capabilities (M0); fall back to /health
        // as a liveness signal and return a minimal CapabilitiesResponse with
        // the desktop's compile-time version. M1 will swap this to the real
        // capabilities endpoint.
        match self.try_capabilities().await {
            Ok(c) => Ok(c),
            Err(ClientError::HttpStatus(404)) => {
                self.health().await?;
                Ok(CapabilitiesResponse {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    schema_version: "v3".to_string(),
                    agents: vec![],
                    features: CapabilityFlags::default(),
                    server_time: String::new(),
                })
            }
            Err(e) => Err(e),
        }
    }

    async fn try_capabilities(&self) -> Result<CapabilitiesResponse, ClientError> {
        let url = format!("{}/api/capabilities", self.base_url);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ClientError::HttpStatus(status.as_u16()));
        }
        resp.json::<CapabilitiesResponse>()
            .await
            .map_err(|e| ClientError::Parse(e.to_string()))
    }

    pub async fn health(&self) -> Result<(), ClientError> {
        let url = format!("{}/api/health", self.base_url);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ClientError::HttpStatus(resp.status().as_u16()));
        }
        Ok(())
    }

    pub async fn list_conversations(
        &self,
        agent_type: Option<AgentType>,
        folder_path: Option<String>,
    ) -> Result<Vec<ConversationSummary>, ClientError> {
        let url = format!("{}/api/list_conversations", self.base_url);
        let body = ListConversationsBody {
            agent_type,
            search: None,
            sort_by: None,
            folder_path,
        };
        self.post_json(&url, &body).await
    }

    pub async fn get_conversation(
        &self,
        agent_type: AgentType,
        conversation_id: String,
    ) -> Result<ConversationDetail, ClientError> {
        let url = format!("{}/api/get_conversation", self.base_url);
        let body = GetConversationBody {
            agent_type,
            conversation_id,
        };
        self.post_json(&url, &body).await
    }

    pub async fn acp_connect(
        &self,
        agent_type: AgentType,
        working_dir: Option<String>,
        session_id: Option<String>,
    ) -> Result<String, ClientError> {
        let url = format!("{}/api/acp_connect", self.base_url);
        let body = AcpConnectBody {
            agent_type,
            working_dir,
            session_id,
        };
        self.post_json(&url, &body).await
    }

    pub async fn acp_prompt(
        &self,
        connection_id: String,
        blocks: Vec<PromptInputBlock>,
        folder_id: Option<i32>,
        conversation_id: Option<i32>,
    ) -> Result<(), ClientError> {
        let url = format!("{}/api/acp_prompt", self.base_url);
        let body = AcpPromptBody {
            connection_id,
            blocks,
            folder_id,
            conversation_id,
        };
        let _: serde_json::Value = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn acp_cancel(&self, connection_id: String) -> Result<(), ClientError> {
        let url = format!("{}/api/acp_cancel", self.base_url);
        let body = AcpConnectionIdBody { connection_id };
        let _: serde_json::Value = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn acp_respond_permission(
        &self,
        connection_id: String,
        request_id: String,
        option_id: String,
    ) -> Result<(), ClientError> {
        let url = format!("{}/api/acp_respond_permission", self.base_url);
        let body = AcpRespondPermissionBody {
            connection_id,
            request_id,
            option_id,
        };
        let _: serde_json::Value = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn acp_set_mode(
        &self,
        connection_id: String,
        mode_id: String,
    ) -> Result<(), ClientError> {
        let url = format!("{}/api/acp_set_mode", self.base_url);
        let body = AcpSetModeBody {
            connection_id,
            mode_id,
        };
        let _: serde_json::Value = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn acp_set_config_option(
        &self,
        connection_id: String,
        config_id: String,
        value_id: String,
    ) -> Result<(), ClientError> {
        let url = format!("{}/api/acp_set_config_option", self.base_url);
        let body = AcpSetConfigOptionBody {
            connection_id,
            config_id,
            value_id,
        };
        let _: serde_json::Value = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn acp_fork(&self, connection_id: String) -> Result<ForkResultInfo, ClientError> {
        let url = format!("{}/api/acp_fork", self.base_url);
        let body = AcpConnectionIdBody { connection_id };
        self.post_json(&url, &body).await
    }

    pub async fn acp_disconnect(&self, connection_id: String) -> Result<(), ClientError> {
        let url = format!("{}/api/acp_disconnect", self.base_url);
        let body = AcpConnectionIdBody { connection_id };
        let _: serde_json::Value = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn acp_touch_connection(
        &self,
        connection_id: String,
    ) -> Result<bool, ClientError> {
        let url = format!("{}/api/acp_touch_connection", self.base_url);
        let body = AcpConnectionIdBody { connection_id };
        self.post_json(&url, &body).await
    }

    pub async fn acp_get_session_snapshot(
        &self,
        connection_id: String,
    ) -> Result<Option<LiveSessionSnapshot>, ClientError> {
        let url = format!("{}/api/acp_get_session_snapshot", self.base_url);
        let body = AcpConnectionIdBody { connection_id };
        self.post_json(&url, &body).await
    }

    async fn post_json<B: serde::Serialize, R: for<'de> serde::Deserialize<'de>>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<R, ClientError> {
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let snippet = resp.text().await.unwrap_or_default();
            return Err(ClientError::HttpStatusWithBody {
                status: status.as_u16(),
                body: snippet,
            });
        }
        resp.json::<R>()
            .await
            .map_err(|e| ClientError::Parse(e.to_string()))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListConversationsBody {
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    folder_path: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GetConversationBody {
    agent_type: AgentType,
    conversation_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcpConnectBody {
    agent_type: AgentType,
    working_dir: Option<String>,
    session_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcpPromptBody {
    connection_id: String,
    blocks: Vec<PromptInputBlock>,
    folder_id: Option<i32>,
    conversation_id: Option<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcpConnectionIdBody {
    connection_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcpRespondPermissionBody {
    connection_id: String,
    request_id: String,
    option_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcpSetModeBody {
    connection_id: String,
    mode_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AcpSetConfigOptionBody {
    connection_id: String,
    config_id: String,
    value_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("network: {0}")]
    Network(String),
    #[error("http status: {0}")]
    HttpStatus(u16),
    #[error("http {status}: {body}")]
    HttpStatusWithBody { status: u16, body: String },
    #[error("parse: {0}")]
    Parse(String),
}
