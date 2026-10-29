//! Bridges maki's permission config to the optional [`maki_cedar`] engine.
//!
//! When `permissions.toml` sets `engine = "cedar"`, [`CedarBackend::build`]
//! produces the engine from a local policy file, a synced `cedar-agent`
//! store, or both. The result plugs into [`super::PermissionManager`] as an
//! extra decision source: a Cedar `forbid` denies, a `permit` allows, and no
//! match defers to maki's own rules and defaults.

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use maki_cedar::{AgentClient, AgentVerdict, CedarDecision, CedarEngine};
use maki_config::{CedarSettings, PolicyEngineKind};
use maki_storage::paths::expand_tilde;
use tracing::{info, warn};

/// How often to re-fetch from a `cedar-agent` when the config leaves it at 0.
const DEFAULT_SYNC_SECS: u64 = 300;
const AGENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Policies evaluated in process, re-synced from the agent on an interval.
pub struct LocalBackend {
    engine: RwLock<Arc<CedarEngine>>,
    /// Present when an `agent_url` contributes policies.
    resync: Option<Resync>,
}

struct Resync {
    settings: CedarSettings,
    every: Duration,
    last: Mutex<Instant>,
}

/// The Cedar decision path wired into the permission manager.
pub enum CedarBackend {
    /// Policies evaluated in-process (local file and/or synced agent policies).
    Local(LocalBackend),
    /// Every call asks a remote `cedar-agent` to decide.
    PerCall(Arc<AgentClient>),
}

impl CedarBackend {
    /// Wrap an already-built engine, for tests that inject a policy set.
    #[cfg(test)]
    pub(crate) fn from_engine(engine: Arc<CedarEngine>) -> Self {
        Self::Local(LocalBackend {
            engine: RwLock::new(engine),
            resync: None,
        })
    }

    /// Build the backend selected by `settings`, or `None` when the builtin
    /// engine is in force. A misconfiguration logs and returns `None` so maki
    /// falls back to its own rules rather than failing closed on every tool.
    pub fn build(settings: &CedarSettings) -> Option<Self> {
        if settings.engine != PolicyEngineKind::Cedar {
            return None;
        }
        if settings.agent_per_call {
            let url = settings.agent_url.as_deref()?;
            return match client(settings, url) {
                Ok(c) => {
                    info!(url, "cedar: per-call authorization via cedar-agent");
                    Some(Self::PerCall(Arc::new(c)))
                }
                Err(e) => {
                    warn!(error = %e, "cedar: agent client build failed, using builtin rules");
                    None
                }
            };
        }

        let engine = compile(settings)?;
        info!("cedar: local policy evaluation active");
        let resync = settings.agent_url.as_ref().map(|_| Resync {
            settings: settings.clone(),
            every: Duration::from_secs(if settings.sync_interval_secs == 0 {
                DEFAULT_SYNC_SECS
            } else {
                settings.sync_interval_secs
            }),
            last: Mutex::new(Instant::now()),
        });
        Some(Self::Local(LocalBackend {
            engine: RwLock::new(Arc::new(engine)),
            resync,
        }))
    }

    /// Decide one `(tool_key, scope)` pair. `PerCall` blocks on the agent,
    /// mapping an error or a no-match to `NoDecision` so a network fault or an
    /// uncovered call defers to maki's rules instead of denying every call.
    pub fn decide(&self, tool_key: &str, scope: Option<&str>) -> CedarDecision {
        match self {
            Self::Local(local) => {
                local.maybe_resync();
                let engine = local.engine.read().map(|e| Arc::clone(&e)).ok();
                match engine {
                    Some(e) => e.decide(tool_key, scope),
                    None => CedarDecision::NoDecision,
                }
            }
            Self::PerCall(client) => match smol::block_on(client.is_authorized(tool_key, scope)) {
                Ok(AgentVerdict::Allow) => CedarDecision::Allow,
                Ok(AgentVerdict::Deny) => CedarDecision::Deny("denied by cedar-agent".to_string()),
                Ok(AgentVerdict::NoMatch) => CedarDecision::NoDecision,
                Err(e) => {
                    warn!(error = %e, tool = tool_key, "cedar-agent query failed, deferring");
                    CedarDecision::NoDecision
                }
            },
        }
    }
}

impl LocalBackend {
    /// Re-fetch from the agent once `every` has elapsed. Lazy (on the next
    /// check) rather than a background task. A failed refresh keeps the last
    /// good engine, so an outage never drops the policy set.
    fn maybe_resync(&self) {
        let Some(r) = &self.resync else { return };
        {
            let Ok(mut last) = r.last.try_lock() else {
                return;
            };
            if last.elapsed() < r.every {
                return;
            }
            *last = Instant::now();
        }
        match compile(&r.settings) {
            Some(engine) => {
                if let Ok(mut slot) = self.engine.write() {
                    *slot = Arc::new(engine);
                    info!("cedar: re-synced policies from agent");
                }
            }
            None => warn!("cedar: re-sync failed, keeping the last good policy set"),
        }
    }
}

fn client(settings: &CedarSettings, url: &str) -> Result<AgentClient, maki_cedar::CedarError> {
    let token = settings.agent_token_file.as_ref().and_then(|p| {
        let path = expand_tilde(p);
        match std::fs::read_to_string(&path) {
            Ok(s) => Some(s.trim().to_string()),
            Err(e) => {
                warn!(path = %path.display(), error = %e, "cedar: cannot read agent_token_file");
                None
            }
        }
    });
    Ok(AgentClient::new(url, AGENT_TIMEOUT)?.with_token(token))
}

fn compile(settings: &CedarSettings) -> Option<CedarEngine> {
    let policies = load_policies(settings)?;
    let schema = load_schema(settings);
    match CedarEngine::from_source(&policies, schema.as_deref()) {
        Ok(e) => Some(e),
        Err(e) => {
            warn!(error = %e, "cedar: policy compile failed, using builtin rules");
            None
        }
    }
}

/// Combine a local policy file with any policies synced from `cedar-agent`.
/// At least one source must yield policy text.
fn load_policies(settings: &CedarSettings) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    if let Some(path) = &settings.policy_file {
        let path = expand_tilde(path);
        match std::fs::read_to_string(&path) {
            Ok(s) => parts.push(s),
            Err(e) => warn!(path = %path.display(), error = %e, "cedar: cannot read policy_file"),
        }
    }

    if let Some(url) = &settings.agent_url {
        match client(settings, url) {
            Ok(client) => match smol::block_on(client.fetch_policies()) {
                Ok(s) => {
                    info!(url, "cedar: synced policies from agent");
                    parts.push(s);
                }
                Err(e) => warn!(url, error = %e, "cedar: policy sync failed"),
            },
            Err(e) => warn!(url, error = %e, "cedar: agent client build failed"),
        }
    }

    if parts.is_empty() {
        warn!("cedar: engine selected but no policy_file or agent_url yielded policies");
        return None;
    }
    Some(parts.join("\n"))
}

fn load_schema(settings: &CedarSettings) -> Option<String> {
    let path = expand_tilde(settings.schema_file.as_ref()?);
    match std::fs::read_to_string(&path) {
        Ok(s) => Some(s),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cedar: cannot read schema_file");
            None
        }
    }
}
