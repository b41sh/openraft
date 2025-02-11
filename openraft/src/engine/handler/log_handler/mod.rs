use crate::engine::engine_impl::EngineOutput;
use crate::engine::Command;
use crate::engine::EngineConfig;
use crate::raft_state::LogStateReader;
use crate::summary::MessageSummary;
use crate::LogId;
use crate::LogIdOptionExt;
use crate::Node;
use crate::NodeId;
use crate::RaftState;

#[cfg(test)] mod calc_purge_upto_test;
#[cfg(test)] mod purge_log_test;

/// Handle raft vote related operations
pub(crate) struct LogHandler<'x, NID, N>
where
    NID: NodeId,
    N: Node,
{
    pub(crate) config: &'x mut EngineConfig<NID>,
    pub(crate) state: &'x mut RaftState<NID, N>,
    pub(crate) output: &'x mut EngineOutput<NID, N>,
}

impl<'x, NID, N> LogHandler<'x, NID, N>
where
    NID: NodeId,
    N: Node,
{
    /// Purge log entries upto `RaftState.purge_upto()`, inclusive.
    #[tracing::instrument(level = "debug", skip_all)]
    pub(crate) fn purge_log(&mut self) {
        let st = &mut self.state;
        let purge_upto = st.purge_upto();

        tracing::info!(
            last_purged_log_id = display(st.last_purged_log_id().summary()),
            purge_upto = display(purge_upto.summary()),
            "purge_log"
        );

        if purge_upto <= st.last_purged_log_id() {
            return;
        }

        let upto = *purge_upto.unwrap();

        st.purge_log(&upto);
        self.output.push_command(Command::PurgeLog { upto });
    }

    /// Update the next log id to purge upto, if more logs can be purged, according to configured
    /// policy.
    ///
    /// This method is called after building a snapshot, because openraft only purge logs that are
    /// already included in snapshot.
    #[tracing::instrument(level = "debug", skip_all)]
    pub(crate) fn update_purge_upto(&mut self) {
        if let Some(purge_upto) = self.calc_purge_upto() {
            debug_assert!(self.state.purge_upto() <= Some(&purge_upto));

            self.state.purge_upto = Some(purge_upto);
        }
    }

    /// Calculate the log id up to which to purge, inclusive.
    ///
    /// Only log included in snapshot will be purged.
    /// It may return None if there is no log to purge.
    ///
    /// `max_keep` specifies the number of applied logs to keep.
    /// `max_keep==0` means every applied log can be purged.
    #[tracing::instrument(level = "debug", skip_all)]
    pub(crate) fn calc_purge_upto(&self) -> Option<LogId<NID>> {
        let st = &self.state;
        let max_keep = self.config.max_in_snapshot_log_to_keep;
        let batch_size = self.config.purge_batch_size;

        let purge_end = self.state.snapshot_meta.last_log_id.next_index().saturating_sub(max_keep);

        tracing::debug!(
            snapshot_last_log_id = debug(self.state.snapshot_meta.last_log_id),
            max_keep,
            "try purge: (-oo, {})",
            purge_end
        );

        if st.last_purged_log_id().next_index() + batch_size > purge_end {
            tracing::debug!(
                snapshot_last_log_id = debug(self.state.snapshot_meta.last_log_id),
                max_keep,
                last_purged_log_id = display(st.last_purged_log_id().summary()),
                batch_size,
                purge_end,
                "no need to purge",
            );
            return None;
        }

        let log_id = self.state.log_ids.get(purge_end - 1);
        debug_assert!(
            log_id.is_some(),
            "log id not found at {}, engine.state:{:?}",
            purge_end - 1,
            st
        );

        log_id
    }
}
