use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const CONTINUATION_CHECKPOINT_MS: u64 = 240_000;
pub const CAPABILITY_DELEGATION_CONTINUATION_V1: &str = "delegation_continuation_v1";

macro_rules! continuation_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl FromStr for $name {
            type Err = &'static str;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err("unknown continuation enum value"),
                }
            }
        }
    };
}

continuation_enum!(ContinuationState {
    Arming => "arming",
    Waiting => "waiting",
    WakePending => "wake_pending",
    Resuming => "resuming",
    Completed => "completed",
    Cancelled => "cancelled",
    Failed => "failed",
});

continuation_enum!(ContinuationWakeReason {
    AllTerminal => "all_terminal",
    AttentionRequired => "attention_required",
    Unavailable => "unavailable",
    Checkpoint => "checkpoint",
});

continuation_enum!(ContinuationFailureCode {
    ArmFailed => "arm_failed",
    SuspendDispatchFailed => "suspend_dispatch_failed",
    SuspendDrainTimeout => "suspend_drain_timeout",
    ParentConnectionLost => "parent_connection_lost",
    PromptDeliveryFailed => "prompt_delivery_failed",
    StateConflict => "state_conflict",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationTaskIds(pub Vec<String>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationWaitingProjection {
    pub conversation_id: i32,
    pub state: ContinuationState,
    pub generation: u64,
    pub armed_at: DateTime<Utc>,
    pub wake_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        ContinuationFailureCode, ContinuationState, ContinuationWakeReason,
        CAPABILITY_DELEGATION_CONTINUATION_V1, CONTINUATION_CHECKPOINT_MS,
    };

    #[test]
    fn continuation_domain_values_round_trip_snake_case() {
        assert_eq!(CONTINUATION_CHECKPOINT_MS, 240_000);
        assert_eq!(
            CAPABILITY_DELEGATION_CONTINUATION_V1,
            "delegation_continuation_v1"
        );

        for (value, expected) in [
            (ContinuationState::Arming, "arming"),
            (ContinuationState::Waiting, "waiting"),
            (ContinuationState::WakePending, "wake_pending"),
            (ContinuationState::Resuming, "resuming"),
            (ContinuationState::Completed, "completed"),
            (ContinuationState::Cancelled, "cancelled"),
            (ContinuationState::Failed, "failed"),
        ] {
            assert_eq!(value.as_str(), expected);
            assert_eq!(ContinuationState::from_str(expected), Ok(value));
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{expected}\"")
            );
        }

        for (value, expected) in [
            (ContinuationWakeReason::AllTerminal, "all_terminal"),
            (
                ContinuationWakeReason::AttentionRequired,
                "attention_required",
            ),
            (ContinuationWakeReason::Unavailable, "unavailable"),
            (ContinuationWakeReason::Checkpoint, "checkpoint"),
        ] {
            assert_eq!(value.as_str(), expected);
            assert_eq!(ContinuationWakeReason::from_str(expected), Ok(value));
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{expected}\"")
            );
        }

        for (value, expected) in [
            (ContinuationFailureCode::ArmFailed, "arm_failed"),
            (
                ContinuationFailureCode::SuspendDispatchFailed,
                "suspend_dispatch_failed",
            ),
            (
                ContinuationFailureCode::SuspendDrainTimeout,
                "suspend_drain_timeout",
            ),
            (
                ContinuationFailureCode::ParentConnectionLost,
                "parent_connection_lost",
            ),
            (
                ContinuationFailureCode::PromptDeliveryFailed,
                "prompt_delivery_failed",
            ),
            (ContinuationFailureCode::StateConflict, "state_conflict"),
        ] {
            assert_eq!(value.as_str(), expected);
            assert_eq!(ContinuationFailureCode::from_str(expected), Ok(value));
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                format!("\"{expected}\"")
            );
        }

        assert!(ContinuationState::from_str("unknown").is_err());
        assert!(ContinuationWakeReason::from_str("unknown").is_err());
        assert!(ContinuationFailureCode::from_str("unknown").is_err());
    }
}
