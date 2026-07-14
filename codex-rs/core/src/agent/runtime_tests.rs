use super::*;
use codex_extension_api::AgentRuntimeFuture;
use codex_extension_api::AgentRuntimeResumeRequest;
use codex_extension_api::AgentRuntimeSpawnRequest;
use codex_extension_api::AgentRuntimeThread;
use pretty_assertions::assert_eq;

struct FakeProvider {
    id: AgentRuntimeId,
}

impl FakeProvider {
    fn new(id: &str) -> Self {
        Self {
            id: AgentRuntimeId::new(id).expect("valid fake runtime id"),
        }
    }
}

impl AgentRuntimeProvider for FakeProvider {
    fn id(&self) -> &AgentRuntimeId {
        &self.id
    }

    fn spawn(
        &self,
        _request: AgentRuntimeSpawnRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        Box::pin(async {
            Err(AgentRuntimeError::new(
                AgentRuntimeErrorKind::UnsupportedOperation,
                "not used by registry tests",
            ))
        })
    }

    fn resume(
        &self,
        _request: AgentRuntimeResumeRequest,
    ) -> AgentRuntimeFuture<'_, Arc<dyn AgentRuntimeThread>> {
        Box::pin(async {
            Err(AgentRuntimeError::new(
                AgentRuntimeErrorKind::UnsupportedOperation,
                "not used by registry tests",
            ))
        })
    }
}

#[test]
fn registry_resolves_codex_as_the_builtin_default_provider() {
    let registry = AgentRuntimeRegistry::default();

    assert!(matches!(
        registry.resolve(&AgentRuntimeId::codex()),
        Ok(ResolvedAgentRuntime::Codex)
    ));
}

#[test]
fn registry_registers_and_resolves_external_provider() {
    let registry = AgentRuntimeRegistry::default();
    let provider: Arc<dyn AgentRuntimeProvider> = Arc::new(FakeProvider::new("pi"));
    registry
        .register(Arc::clone(&provider))
        .expect("register provider");

    let ResolvedAgentRuntime::External(resolved) = registry
        .resolve(&AgentRuntimeId::new("pi").expect("valid runtime id"))
        .expect("resolve provider")
    else {
        panic!("pi should resolve as an external provider");
    };
    assert!(Arc::ptr_eq(&provider, &resolved));
}

#[test]
fn registry_rejects_duplicate_and_reserved_runtime_ids() {
    let registry = AgentRuntimeRegistry::default();
    registry
        .register(Arc::new(FakeProvider::new("pi")))
        .expect("register provider");

    let duplicate = registry
        .register(Arc::new(FakeProvider::new("pi")))
        .expect_err("duplicate provider should fail");
    let reserved = registry
        .register(Arc::new(FakeProvider::new("codex")))
        .expect_err("reserved provider should fail");

    assert_eq!(duplicate.kind, AgentRuntimeErrorKind::DuplicateRuntime);
    assert_eq!(reserved.kind, AgentRuntimeErrorKind::DuplicateRuntime);
}

#[test]
fn registry_reports_unknown_runtime() {
    let registry = AgentRuntimeRegistry::default();
    let error = registry
        .resolve(&AgentRuntimeId::new("missing").expect("valid runtime id"))
        .err()
        .expect("missing runtime should fail");

    assert_eq!(error.kind, AgentRuntimeErrorKind::UnknownRuntime);
}
