use crate::engine::engine_impl::EngineOutput;
use crate::engine::Command;
use crate::engine::EngineConfig;
use crate::Node;
use crate::NodeId;
use crate::RaftState;
use crate::ServerState;

#[cfg(test)] mod update_server_state_test;

/// Handle raft server-state related operations
pub(crate) struct ServerStateHandler<'st, NID, N>
where
    NID: NodeId,
    N: Node,
{
    pub(crate) config: &'st EngineConfig<NID>,
    pub(crate) state: &'st mut RaftState<NID, N>,
    pub(crate) output: &'st mut EngineOutput<NID, N>,
}

impl<'st, NID, N> ServerStateHandler<'st, NID, N>
where
    NID: NodeId,
    N: Node,
{
    /// Re-calculate the server-state, if it changed, update the `server_state` field and dispatch
    /// commands to inform a runtime.
    pub(crate) fn update_server_state_if_changed(&mut self) {
        let server_state = self.state.calc_server_state(&self.config.id);

        tracing::debug!(
            id = display(self.config.id),
            prev_server_state = debug(self.state.server_state),
            server_state = debug(server_state),
            "update_server_state_if_changed"
        );

        if self.state.server_state == server_state {
            return;
        }

        let was_leader = self.state.server_state == ServerState::Leader;
        let is_leader = server_state == ServerState::Leader;

        if !was_leader && is_leader {
            self.output.push_command(Command::BecomeLeader);
        } else if was_leader && !is_leader {
            self.output.push_command(Command::QuitLeader);
        } else {
            // nothing to do
        }

        self.state.server_state = server_state;
    }
}
