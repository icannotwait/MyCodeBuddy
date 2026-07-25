use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegateAccessMode {
    ViewerOnly,
    Interactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegateAccessReason {
    TaskRunning,
    ParentTurnActive,
    StateUnknown,
}

impl DelegateAccessReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskRunning => "task_running",
            Self::ParentTurnActive => "parent_turn_active",
            Self::StateUnknown => "state_unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegateAccessState {
    pub mode: DelegateAccessMode,
    pub reason: Option<DelegateAccessReason>,
    pub parent_id: Option<i32>,
}

impl DelegateAccessState {
    pub const fn interactive(parent_id: Option<i32>) -> Self {
        Self {
            mode: DelegateAccessMode::Interactive,
            reason: None,
            parent_id,
        }
    }

    pub const fn viewer_only(reason: DelegateAccessReason, parent_id: Option<i32>) -> Self {
        Self {
            mode: DelegateAccessMode::ViewerOnly,
            reason: Some(reason),
            parent_id,
        }
    }
}
