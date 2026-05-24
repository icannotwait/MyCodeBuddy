//! Walk the conversation parent chain to compute delegation depth.
//!
//! The walker is generic over an async closure so the broker can plug in a
//! real DB lookup in production and a stub `Vec<(id, parent_id)>` in tests
//! without any extra trait plumbing.
//!
//! `cap` saturates the walk so a corrupted chain (cycle, deep history) can't
//! cause unbounded DB load. Callers pass `depth_limit + 1` — that's all the
//! broker ever needs to decide rejection.

use std::future::Future;

use crate::acp::delegation::types::DelegationError;

pub async fn compute_depth<F, Fut>(
    start: i32,
    mut parent_resolver: F,
    cap: u32,
) -> Result<u32, DelegationError>
where
    F: FnMut(i32) -> Fut,
    Fut: Future<Output = Result<Option<i32>, DelegationError>>,
{
    let mut current = start;
    let mut depth = 0u32;
    while depth < cap {
        match parent_resolver(current).await? {
            None => return Ok(depth),
            Some(parent) => {
                current = parent;
                depth += 1;
            }
        }
    }
    Ok(depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn fake_chain(n: usize) -> Vec<i32> {
        (0..n as i32).collect()
    }

    fn parent_of(chain: &[i32], id: i32) -> Result<Option<i32>, DelegationError> {
        let idx = chain
            .iter()
            .position(|c| *c == id)
            .expect("test resolver called with id not in chain");
        if idx == 0 {
            Ok(None)
        } else {
            Ok(Some(chain[idx - 1]))
        }
    }

    #[tokio::test]
    async fn depth_of_root_is_zero() {
        let chain = fake_chain(1);
        let resolver = |id: i32| {
            let chain = chain.clone();
            async move { parent_of(&chain, id) }
        };
        let depth = compute_depth(chain[0], resolver, 8).await.unwrap();
        assert_eq!(depth, 0);
    }

    #[tokio::test]
    async fn depth_of_grandchild_is_two() {
        let chain = fake_chain(3); // root -> mid -> leaf
        let resolver = |id: i32| {
            let chain = chain.clone();
            async move { parent_of(&chain, id) }
        };
        let depth = compute_depth(chain[2], resolver, 8).await.unwrap();
        assert_eq!(depth, 2);
    }

    #[tokio::test]
    async fn saturates_at_cap_without_walking_full_chain() {
        let chain = fake_chain(20);
        let calls = AtomicU32::new(0);
        let resolver = |id: i32| {
            calls.fetch_add(1, Ordering::SeqCst);
            let chain = chain.clone();
            async move { parent_of(&chain, id) }
        };
        let depth = compute_depth(chain[19], resolver, 3).await.unwrap();
        assert_eq!(depth, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn resolver_error_propagates() {
        let resolver = |_id: i32| async {
            Err::<Option<i32>, _>(DelegationError::SubagentRuntimeError("db down".into()))
        };
        let err = compute_depth(42, resolver, 8).await.unwrap_err();
        assert!(matches!(err, DelegationError::SubagentRuntimeError(_)));
    }
}
