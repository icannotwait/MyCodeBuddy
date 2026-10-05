//! Fake room trajectory. No prompt body and no tool call is emitted.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trajectory {
    pub member_turns: u32,
    pub moderator_turns: u32,
    pub phases: u32,
    pub accepted_messages: u32,
    pub prompt_bodies: u32,
    pub tool_calls: u32,
    pub business_events: u32,
    pub cleanup_proofs: u32,
    pub terminal: bool,
}

struct FakeRoom {
    trajectory: Trajectory,
}

impl FakeRoom {
    fn new() -> Self {
        Self {
            trajectory: Trajectory {
                member_turns: 0,
                moderator_turns: 0,
                phases: 0,
                accepted_messages: 0,
                prompt_bodies: 0,
                tool_calls: 0,
                business_events: 0,
                cleanup_proofs: 0,
                terminal: false,
            },
        }
    }

    fn member(&mut self) {
        self.trajectory.member_turns += 1;
    }

    fn moderator(&mut self) {
        self.trajectory.moderator_turns += 1;
    }

    fn close_phase(&mut self) {
        self.trajectory.phases += 1;
    }

    fn accept(&mut self) {
        self.trajectory.accepted_messages += 1;
    }

    fn finish(mut self) -> Trajectory {
        self.trajectory.cleanup_proofs += 1;
        self.trajectory.business_events += 1;
        self.trajectory.terminal = true;
        self.trajectory
    }
}

pub fn exercise_room() -> Trajectory {
    let mut room = FakeRoom::new();
    room.member();
    room.member();
    room.moderator();
    room.close_phase();
    room.close_phase();
    room.close_phase();
    room.accept();
    room.accept();
    room.finish()
}
