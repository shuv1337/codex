use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use codex_extension_api::AgentRuntimeError;
use codex_extension_api::AgentRuntimeErrorKind;
use codex_extension_api::AgentRuntimeId;
use codex_extension_api::AgentRuntimeProvider;

pub(crate) enum ResolvedAgentRuntime {
    Codex,
    External(Arc<dyn AgentRuntimeProvider>),
}

/// Registry for built-in Codex and externally supplied agent runtime providers.
///
/// Native Codex is reserved and resolved without a trait object because its spawn path owns the
/// host session construction. External providers register by stable ID before threads are
/// spawned. Registration is synchronized so app-server initialization and tests do not need a
/// mutable `ThreadManager` handle.
#[derive(Default)]
pub(crate) struct AgentRuntimeRegistry {
    external: RwLock<HashMap<AgentRuntimeId, Arc<dyn AgentRuntimeProvider>>>,
}

impl AgentRuntimeRegistry {
    pub(crate) fn register(
        &self,
        provider: Arc<dyn AgentRuntimeProvider>,
    ) -> Result<(), AgentRuntimeError> {
        let runtime_id = provider.id().clone();
        if runtime_id.is_codex() {
            return Err(AgentRuntimeError::new(
                AgentRuntimeErrorKind::DuplicateRuntime,
                "agent runtime `codex` is reserved for the built-in provider",
            ));
        }
        let mut providers = self.external.write().map_err(|_| {
            AgentRuntimeError::new(
                AgentRuntimeErrorKind::Internal,
                "agent runtime registry write lock is poisoned",
            )
        })?;
        if providers.contains_key(&runtime_id) {
            return Err(AgentRuntimeError::new(
                AgentRuntimeErrorKind::DuplicateRuntime,
                format!("agent runtime `{runtime_id}` is already registered"),
            ));
        }
        providers.insert(runtime_id, provider);
        Ok(())
    }

    pub(crate) fn resolve(
        &self,
        runtime_id: &AgentRuntimeId,
    ) -> Result<ResolvedAgentRuntime, AgentRuntimeError> {
        if runtime_id.is_codex() {
            return Ok(ResolvedAgentRuntime::Codex);
        }
        let providers = self.external.read().map_err(|_| {
            AgentRuntimeError::new(
                AgentRuntimeErrorKind::Internal,
                "agent runtime registry read lock is poisoned",
            )
        })?;
        providers
            .get(runtime_id)
            .cloned()
            .map(ResolvedAgentRuntime::External)
            .ok_or_else(|| {
                AgentRuntimeError::new(
                    AgentRuntimeErrorKind::UnknownRuntime,
                    format!("unknown agent runtime `{runtime_id}`"),
                )
            })
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
