use std::io;
use std::sync::LazyLock;
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::runtime::{Builder, Runtime};

use crate::{ACTION, CedarError, PRINCIPAL, TOOL_TYPE};

/// Default cedar-agent listen port (see permitio/cedar-agent).
pub const DEFAULT_AGENT_PORT: u16 = 8180;

const RUNTIME_THREAD_NAME: &str = "maki-cedar";

/// The permission check is sync and runs from tokio workers, the blocking
/// pool and the Lua `LocalSet` alike, where `block_in_place` panics. A private
/// runtime serves every request, so the caller only ever waits on a channel.
static RUNTIME: LazyLock<io::Result<Runtime>> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name(RUNTIME_THREAD_NAME)
        .enable_all()
        .build()
});

/// One policy as cedar-agent stores and returns it on `GET /v1/policies`.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentPolicy {
    pub id: String,
    pub content: String,
}

/// The body cedar-agent expects on `POST /v1/is_authorized`.
#[derive(Debug, Serialize)]
struct AuthorizationQuery {
    principal: String,
    action: String,
    resource: String,
    context: serde_json::Value,
}

/// cedar-agent's answer to an authorization query.
#[derive(Debug, Deserialize)]
struct AuthorizationResponse {
    decision: String,
    /// cedar-store reports whether any policy matched; cedar-agent does not,
    /// so fall back to the diagnostics' reasons.
    #[serde(default)]
    matched: Option<bool>,
    #[serde(default)]
    diagnostics: Option<Diagnostics>,
}

#[derive(Debug, Default, Deserialize)]
struct Diagnostics {
    #[serde(default)]
    reason: Vec<String>,
}

/// A per-call answer: allow, an explicit deny, or no policy matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentVerdict {
    Allow,
    Deny,
    NoMatch,
}

/// A client for a remote `cedar-agent` policy store.
///
/// The primary mode is to [`fetch_policies`](Self::fetch_policies) and
/// [`fetch_schema`](Self::fetch_schema) once at startup (and periodically),
/// then evaluate locally with a [`crate::CedarEngine`] — a network blip never
/// stalls a tool call. [`is_authorized`](Self::is_authorized) is the fallback
/// that asks the agent to decide per call, for deployments that want the agent
/// to own evaluation and have entity data maki does not.
#[derive(Clone)]
pub struct AgentClient {
    base_url: String,
    http: reqwest::Client,
    token: Option<String>,
    project: String,
}

impl AgentClient {
    pub fn new(base_url: impl Into<String>, timeout: Duration) -> Result<Self, CedarError> {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| CedarError::Request(e.to_string()))?;
        Ok(Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http,
            token: None,
            project: String::new(),
        })
    }

    /// Send `Authorization: Bearer <token>` on every request.
    #[must_use]
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|t| !t.is_empty());
        self
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    /// Fetch all policies and concatenate their `content` into one Cedar
    /// source string ready for [`crate::CedarEngine::from_source`].
    pub fn fetch_policies(&self) -> Result<String, CedarError> {
        let this = self.clone();
        block_on(async move { this.fetch_policies_inner().await })?
    }

    async fn fetch_policies_inner(&self) -> Result<String, CedarError> {
        let url = format!("{}/v1/policies", self.base_url);
        let policies: Vec<AgentPolicy> = self
            .auth(self.http.get(&url))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| CedarError::Request(e.to_string()))?
            .json()
            .await
            .map_err(|e| CedarError::Policies(e.to_string()))?;
        // Keep the store's id as an @id annotation so a deny names the policy
        // the user wrote (deny_reason reads it), not Cedar's `policy<N>`.
        Ok(policies
            .into_iter()
            .map(|p| {
                if p.content.trim_start().starts_with("@id(") {
                    p.content
                } else {
                    format!("@id({})\n{}", quote(&p.id), p.content)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Fetch the schema, if the agent has one. cedar-agent serves JSON schema;
    /// the caller passes it to a [`crate::CedarEngine`] built from JSON, or
    /// omits it. Returns `None` when the agent has no schema set.
    pub fn fetch_schema_json(&self) -> Result<Option<serde_json::Value>, CedarError> {
        let this = self.clone();
        block_on(async move { this.fetch_schema_json_inner().await })?
    }

    async fn fetch_schema_json_inner(&self) -> Result<Option<serde_json::Value>, CedarError> {
        let url = format!("{}/v1/schema", self.base_url);
        let resp = self
            .auth(self.http.get(&url))
            .send()
            .await
            .map_err(|e| CedarError::Request(e.to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp
            .error_for_status()
            .map_err(|e| CedarError::Request(e.to_string()))?;
        let schema: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CedarError::Schema(e.to_string()))?;
        if schema.is_null() {
            Ok(None)
        } else {
            Ok(Some(schema))
        }
    }

    /// Fallback: ask cedar-agent to decide one `(tool, scope)` directly.
    /// Returns Allow, an explicit Deny, or NoMatch (no policy applied). Only used when the
    /// local-sync path is disabled.
    pub fn is_authorized(
        &self,
        tool_key: &str,
        scope: Option<&str>,
    ) -> Result<AgentVerdict, CedarError> {
        let this = self.clone();
        let tool_key = tool_key.to_owned();
        let scope = scope.map(str::to_owned);
        block_on(async move { this.is_authorized_inner(&tool_key, scope.as_deref()).await })?
    }

    /// Set the project sent as `context.project` (see [`crate::project_of`]).
    #[must_use]
    pub fn with_project(mut self, project: impl Into<String>) -> Self {
        self.project = project.into();
        self
    }

    async fn is_authorized_inner(
        &self,
        tool_key: &str,
        scope: Option<&str>,
    ) -> Result<AgentVerdict, CedarError> {
        let query = AuthorizationQuery {
            principal: PRINCIPAL.to_string(),
            action: ACTION.to_string(),
            resource: format!("{TOOL_TYPE}::{}", quote(tool_key)),
            context: serde_json::json!({
                crate::CTX_TOOL: tool_key,
                crate::CTX_SCOPE: scope.unwrap_or(""),
                crate::CTX_PROJECT: self.project,
            }),
        };
        let url = format!("{}/v1/is_authorized", self.base_url);
        let resp: AuthorizationResponse = self
            .auth(self.http.post(&url))
            .json(&query)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| CedarError::Request(e.to_string()))?
            .json()
            .await
            .map_err(|e| CedarError::Request(e.to_string()))?;
        if resp.decision.eq_ignore_ascii_case("allow") {
            return Ok(AgentVerdict::Allow);
        }
        // Cedar is two-valued: Deny with no matching policy means "no rule".
        let matched = resp
            .matched
            .unwrap_or_else(|| resp.diagnostics.is_some_and(|d| !d.reason.is_empty()));
        Ok(if matched {
            AgentVerdict::Deny
        } else {
            AgentVerdict::NoMatch
        })
    }
}

fn block_on<T: Send + 'static>(
    future: impl Future<Output = T> + Send + 'static,
) -> Result<T, CedarError> {
    let runtime = RUNTIME
        .as_ref()
        .map_err(|e| CedarError::Request(format!("cedar runtime failed to start: {e}")))?;
    let (tx, rx) = mpsc::sync_channel(1);
    runtime.spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.recv()
        .map_err(|_| CedarError::Request("cedar request task stopped".to_string()))
}

fn quote(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::{AgentPolicy, quote};

    #[test]
    fn policies_deserialize_from_agent_shape() {
        let json = r#"[{"id":"a","content":"permit(principal, action, resource);"}]"#;
        let got: Vec<AgentPolicy> = serde_json::from_str(json).expect("parse");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "a");
        assert!(got[0].content.starts_with("permit"));
    }

    #[test]
    fn quote_escapes_specials() {
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}

#[cfg(test)]
mod store_compat {
    //! A one-shot std-only HTTP stub (no new dev-dependency) standing in for
    //! cedar-store / cedar-agent, to pin the wire behaviour.
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{AgentClient, AgentVerdict};

    /// Serve one request with `body`; report the Authorization header seen.
    fn stub(body: &'static str) -> (String, mpsc::Receiver<Option<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let (mut auth, mut len) = (None, 0usize);
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let t = line.trim_end();
                if t.is_empty() {
                    break;
                }
                let lower = t.to_ascii_lowercase();
                if let Some(v) = lower.strip_prefix("authorization: ") {
                    auth = Some(t[t.len() - v.len()..].to_string());
                }
                if let Some(v) = lower.strip_prefix("content-length: ") {
                    len = v.parse().unwrap();
                }
            }
            let mut b = vec![0; len];
            r.read_exact(&mut b).unwrap();
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            tx.send(auth).unwrap();
        });
        (url, rx)
    }

    fn client(url: &str, token: Option<&str>) -> AgentClient {
        AgentClient::new(url, Duration::from_secs(5))
            .unwrap()
            .with_token(token.map(str::to_string))
    }

    #[test]
    fn sends_bearer_token_when_configured() {
        let (url, rx) = stub("[]");
        client(&url, Some("s3cret")).fetch_policies().unwrap();
        assert_eq!(rx.recv().unwrap().as_deref(), Some("Bearer s3cret"));
    }

    #[test]
    fn sends_no_authorization_without_token() {
        let (url, rx) = stub("[]");
        client(&url, None).fetch_policies().unwrap();
        assert_eq!(rx.recv().unwrap(), None);
    }

    #[test]
    fn deny_without_reason_is_no_match() {
        // cedar-agent shape: no `matched`, empty reasons.
        let (url, _rx) = stub(r#"{"decision":"Deny","diagnostics":{"reason":[],"errors":[]}}"#);
        let v = client(&url, None).is_authorized("bash", Some("ls")).unwrap();
        assert_eq!(v, AgentVerdict::NoMatch);
    }

    #[test]
    fn deny_with_reason_is_deny() {
        let (url, _rx) =
            stub(r#"{"decision":"Deny","diagnostics":{"reason":["deny-sudo"],"errors":[]}}"#);
        let v = client(&url, None).is_authorized("bash", Some("sudo ls")).unwrap();
        assert_eq!(v, AgentVerdict::Deny);
    }

    #[test]
    fn cedar_store_matched_flag_wins() {
        let (url, _rx) =
            stub(r#"{"decision":"Deny","matched":false,"diagnostics":{"reason":[],"errors":[]}}"#);
        let v = client(&url, None).is_authorized("bash", Some("ls")).unwrap();
        assert_eq!(v, AgentVerdict::NoMatch);
    }
}
