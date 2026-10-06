//! P01 test helpers. They map stable labels onto the protocol types and do
//! not copy product algorithms.

use std::fmt::Debug;
use std::str::FromStr;

use roundtable_protocol::{
    BudgetsV1, DurationMs, Fence, Hash256, MemberKind, MemberResultV1, ParticipantV1, QuotasV1,
    RoundtableConfigV1, SafeInt, SourceRefV1, StrategyType, StrategyV1, TimeoutsV1,
};

pub fn config(n: u32, r: u32, c: u32) -> RoundtableConfigV1 {
    let participants = (0..n)
        .map(|ordinal| ParticipantV1 {
            ordinal,
            role: format!("role-{ordinal}"),
            provider_ref: "provider:test".to_string(),
            model: None,
            effort: None,
            agent: None,
        })
        .collect();
    RoundtableConfigV1 {
        schema_version: 1,
        topic: "圆桌契约".to_string(),
        display_name: None,
        workspace_id: "workspace-test".to_string(),
        source_refs: vec![SourceRefV1 {
            snapshot_id: id("snapshot"),
            base_commit: None,
        }],
        participants,
        moderator_ordinal: 0,
        strategy: StrategyV1 {
            type_name: StrategyType::PhasedRounds,
            version: 1,
            critique_rounds: r,
        },
        concurrency: c,
        strict_snapshot_v1: true,
        budgets: BudgetsV1 {
            room_budget: DurationMs(900_000),
            phase_budget: DurationMs(450_000),
        },
        timeouts: TimeoutsV1 {
            attempt_timeout: DurationMs(225_000),
        },
        quotas: QuotasV1 {
            output_byte_limit: SafeInt(8 * 1024),
            input_byte_limit: SafeInt(16 * 1024),
            interjection_byte_limit: SafeInt(16 * 1024),
        },
    }
}

pub fn id<T: FromStr>(label: &str) -> T
where
    T::Err: Debug,
{
    let text = stable_uuid_string(label);
    T::from_str(&text).unwrap_or_else(|err| {
        panic!(
            "stable id {label:?} is not a {}: {err:?}",
            std::any::type_name::<T>()
        )
    })
}

pub fn member(kind: MemberKind, claim_count: usize) -> MemberResultV1 {
    let mut result = MemberResultV1::proposal_skeleton();
    result.kind = kind;
    result.summary = "成员摘要".to_string();
    result.claims = (0..claim_count)
        .map(|index| roundtable_protocol::ClaimV1::basic(format!("c{index}")))
        .collect();
    if kind == MemberKind::Abstain {
        result.reason = Some("成员弃权".to_string());
    }
    result
}

pub fn fence(epoch: u64) -> Fence {
    Fence {
        boot_epoch: roundtable_protocol::Epoch(epoch),
        run_epoch: roundtable_protocol::Epoch(epoch),
        phase_id: id("phase"),
        phase_revision: roundtable_protocol::Revision(1),
        attempt_id: id("attempt"),
        binding_id: id("binding"),
        incarnation: id("incarnation"),
        context_hash: Hash256::from_bytes([0x11; 32]),
        policy_hash: Hash256::from_bytes([0x22; 32]),
    }
}

fn stable_uuid_string(label: &str) -> String {
    let mut state = [0u8; 16];
    for (i, byte) in b"roundtable-plan-1-2-test-id".iter().enumerate() {
        state[i % 16] ^= byte.wrapping_add(i as u8);
    }
    for (i, byte) in label.as_bytes().iter().enumerate() {
        state[i % 16] = state[i % 16]
            .wrapping_mul(31)
            .wrapping_add(*byte)
            .wrapping_add(i as u8);
        state[(i + 7) % 16] ^= byte.rotate_left((i % 8) as u32);
    }
    state[6] = (state[6] & 0x0f) | 0x40;
    state[8] = (state[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        state[0],
        state[1],
        state[2],
        state[3],
        state[4],
        state[5],
        state[6],
        state[7],
        state[8],
        state[9],
        state[10],
        state[11],
        state[12],
        state[13],
        state[14],
        state[15]
    )
}
