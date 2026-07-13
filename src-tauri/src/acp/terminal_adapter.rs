//! Per-agent terminal adapter boundary for ACP launch and request shaping.
//!
//! Adapters may only *add* launch-env keys and validate shells; they never
//! receive a mutable copy of the full runtime environment (so they cannot
//! delete credentials). Codeg's shell declarations always win at finalize time.

use std::collections::BTreeMap;

use sacp::schema::{CreateTerminalRequest, Meta};

use crate::acp::error::AcpError;
use crate::acp::terminal_runtime::TerminalRuntimeError;
use crate::models::agent::AgentType;
use crate::terminal::shell::ResolvedShellSpec;

/// Agent-specific hooks around terminal shell selection and request shape.
pub trait AcpTerminalAdapter: Send + Sync {
    fn validate_shell(&self, _shell: &ResolvedShellSpec) -> Result<(), AcpError> {
        Ok(())
    }

    /// Extra env vars to merge *before* Codeg's authoritative shell declarations.
    ///
    /// Returning additions (not a full env) prevents an adapter from deleting
    /// credentials or rewriting unrelated agent configuration.
    fn agent_launch_env(
        &self,
        _shell: &ResolvedShellSpec,
    ) -> Result<BTreeMap<String, String>, AcpError> {
        Ok(BTreeMap::new())
    }

    fn agent_metadata(&self, _shell: &ResolvedShellSpec) -> Result<Meta, AcpError> {
        Ok(Meta::default())
    }

    fn normalize_terminal_request(
        &self,
        request: CreateTerminalRequest,
    ) -> Result<CreateTerminalRequest, TerminalRuntimeError> {
        Ok(request)
    }
}

/// Default no-op adapter used for every agent until a specialized one exists.
pub struct GenericTerminalAdapter;

impl AcpTerminalAdapter for GenericTerminalAdapter {}

static GENERIC_TERMINAL_ADAPTER: GenericTerminalAdapter = GenericTerminalAdapter;

/// Resolve the terminal adapter for `agent_type`.
///
/// Currently every agent uses the generic no-op implementation. Do not add
/// agent-specific matches here until a later task introduces them.
pub fn adapter_for(_agent_type: AgentType) -> &'static dyn AcpTerminalAdapter {
    &GENERIC_TERMINAL_ADAPTER
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::shell::test_support::pwsh_spec as test_pwsh_spec;
    use sacp::schema::SessionId;

    #[test]
    fn generic_adapter_preserves_request_and_shell() {
        let adapter = adapter_for(AgentType::Grok);
        let request = CreateTerminalRequest::new(SessionId::new("s"), "Get-Location");
        let normalized = adapter.normalize_terminal_request(request.clone()).unwrap();
        assert_eq!(normalized, request);
        assert!(adapter.validate_shell(&test_pwsh_spec()).is_ok());
    }
}
