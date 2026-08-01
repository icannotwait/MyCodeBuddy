pub mod coordinator;
mod prompt;
#[allow(dead_code)] // Task 3 wires this fence into the listener and coordinator.
mod release_fence;
pub mod store;
pub mod types;

#[allow(unused_imports)]
pub(crate) use release_fence::{
    foreground_mcp_release_fence, ForegroundMcpReleaseOutcome, ForegroundMcpReleaseOwner,
    ForegroundMcpReleaseWaiter,
};

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub(crate) use prompt::{
    build_continuation_prompt_text, filter_internal_continuation_turns, internal_prompt_marker,
    DelegationContinuationOrigin,
};
