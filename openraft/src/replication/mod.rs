//! Replication stream.

mod replication_session_id;

use std::fmt::Debug;
use std::fmt::Formatter;
use std::io::SeekFrom;
use std::sync::Arc;

use futures::future::FutureExt;
pub(crate) use replication_session_id::ReplicationSessionId;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncSeek;
use tokio::io::AsyncSeekExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio::time::timeout;
use tokio::time::Duration;
use tracing_futures::Instrument;

use crate::config::Config;
use crate::error::HigherVote;
use crate::error::RPCError;
use crate::error::ReplicationError;
use crate::error::Timeout;
use crate::log_id::LogIdOptionExt;
use crate::log_id_range::LogIdRange;
use crate::raft::AppendEntriesRequest;
use crate::raft::AppendEntriesResponse;
use crate::raft::InstallSnapshotRequest;
use crate::raft::RaftMsg;
use crate::storage::RaftLogReader;
use crate::storage::Snapshot;
use crate::ErrorSubject;
use crate::ErrorVerb;
use crate::LogId;
use crate::MessageSummary;
use crate::Node;
use crate::NodeId;
use crate::RPCTypes;
use crate::RaftNetwork;
use crate::RaftNetworkFactory;
use crate::RaftStorage;
use crate::RaftTypeConfig;
use crate::ToStorageResult;

/// The handle to a spawned replication stream.
pub(crate) struct ReplicationHandle<NID, N, S>
where
    NID: NodeId,
    N: Node,
    S: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    /// The spawn handle the `ReplicationCore` task.
    pub(crate) join_handle: JoinHandle<()>,

    /// The channel used for communicating with the replication task.
    pub(crate) tx_repl: mpsc::UnboundedSender<Replicate<NID, N, S>>,
}

/// A task responsible for sending replication events to a target follower in the Raft cluster.
///
/// NOTE: we do not stack replication requests to targets because this could result in
/// out-of-order delivery. We always buffer until we receive a success response, then send the
/// next payload from the buffer.
pub(crate) struct ReplicationCore<C: RaftTypeConfig, N: RaftNetworkFactory<C>, S: RaftStorage<C>> {
    /// The ID of the target Raft node which replication events are to be sent to.
    target: C::NodeId,

    /// Identifies which session this replication belongs to.
    session_id: ReplicationSessionId<C::NodeId>,

    /// A channel for sending events to the RaftCore.
    #[allow(clippy::type_complexity)]
    tx_raft_core: mpsc::UnboundedSender<RaftMsg<C, N, S>>,

    /// A channel for receiving events from the RaftCore.
    rx_repl: mpsc::UnboundedReceiver<Replicate<C::NodeId, C::Node, S::SnapshotData>>,

    /// The `RaftNetwork` interface.
    network: N::Network,

    /// The `RaftLogReader` of a `RaftStorage` interface.
    log_reader: S::LogReader,

    /// The Raft's runtime config.
    config: Arc<Config>,

    /// The log id of the highest log entry which is known to be committed in the cluster.
    committed: Option<LogId<C::NodeId>>,

    /// Last matching log id on a follower/learner
    matching: Option<LogId<C::NodeId>>,

    /// Next replication action to run.
    next_action: Option<Data<C::NodeId, C::Node, S::SnapshotData>>,
}

impl<C: RaftTypeConfig, N: RaftNetworkFactory<C>, S: RaftStorage<C>> ReplicationCore<C, N, S> {
    /// Spawn a new replication task for the target node.
    #[tracing::instrument(level = "trace", skip_all,fields(target=display(target), session_id=display(session_id)))]
    #[allow(clippy::type_complexity)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn(
        target: C::NodeId,
        session_id: ReplicationSessionId<C::NodeId>,
        config: Arc<Config>,
        committed: Option<LogId<C::NodeId>>,
        matching: Option<LogId<C::NodeId>>,
        network: N::Network,
        log_reader: S::LogReader,
        tx_raft_core: mpsc::UnboundedSender<RaftMsg<C, N, S>>,
        span: tracing::Span,
    ) -> ReplicationHandle<C::NodeId, C::Node, S::SnapshotData> {
        tracing::debug!(
            session_id = display(&session_id),
            target = display(&target),
            committed = display(committed.summary()),
            matching = debug(&matching),
            "spawn replication"
        );

        // other component to ReplicationStream
        let (tx_repl, rx_repl) = mpsc::unbounded_channel();

        let this = Self {
            target,
            session_id,
            network,
            log_reader,
            config,
            committed,
            matching,
            tx_raft_core,
            rx_repl,
            next_action: None,
        };

        let join_handle = tokio::spawn(this.main().instrument(span));

        ReplicationHandle { join_handle, tx_repl }
    }

    #[tracing::instrument(level="debug", skip(self), fields(session=%self.session_id, target=display(self.target), cluster=%self.config.cluster_name))]
    async fn main(mut self) {
        loop {
            let action = std::mem::replace(&mut self.next_action, None);

            let mut repl_id = 0;

            let res = match action {
                None => Ok(()),
                Some(Data { id, payload: r_action }) => {
                    repl_id = id;
                    match r_action {
                        Payload::Logs(log_id_range) => self.send_log_entries(id, log_id_range).await,
                        Payload::Snapshot(snapshot) => self.stream_snapshot(id, snapshot).await,
                    }
                }
            };

            match res {
                Ok(_x) => {}
                Err(err) => {
                    tracing::warn!(error=%err, "error replication to target={}", self.target);

                    match err {
                        ReplicationError::Closed => {
                            return;
                        }
                        ReplicationError::HigherVote(h) => {
                            let _ = self.tx_raft_core.send(RaftMsg::HigherVote {
                                target: self.target,
                                higher: h.higher,
                                vote: self.session_id.vote,
                            });
                            return;
                        }
                        ReplicationError::StorageError(err) => {
                            tracing::error!(error=%err, "error replication to target={}", self.target);

                            // TODO: report this error
                            let _ = self.tx_raft_core.send(RaftMsg::ReplicationFatal);
                            return;
                        }
                        ReplicationError::RPCError(err) => {
                            tracing::error!(err = display(&err), "RPCError");
                            let _ = self.tx_raft_core.send(RaftMsg::UpdateReplicationProgress {
                                target: self.target,
                                id: repl_id,
                                result: Err(err.to_string()),
                                session_id: self.session_id,
                            });
                        }
                    };
                }
            };

            let res = self.drain_events().await;
            match res {
                Ok(_x) => {}
                Err(err) => match err {
                    ReplicationError::Closed => {
                        return;
                    }

                    _ => {
                        unreachable!("no other error expected but: {:?}", err);
                    }
                },
            }
        }
    }

    /// Send an AppendEntries RPC to the target.
    ///
    /// This request will timeout if no response is received within the
    /// configured heartbeat interval.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn send_log_entries(
        &mut self,
        id: u64,
        req: LogIdRange<C::NodeId>,
    ) -> Result<(), ReplicationError<C::NodeId, C::Node>> {
        tracing::debug!(id = display(id), send_req = display(&req), "send_log_entries",);

        let start = req.prev_log_id.next_index();
        let end = req.last_log_id.next_index();

        let logs = if start == end {
            vec![]
        } else {
            let logs = self.log_reader.try_get_log_entries(start..end).await?;
            debug_assert_eq!(
                logs.len(),
                (end - start) as usize,
                "expect logs {}..{} but got only {} entries",
                start,
                end,
                logs.len()
            );
            logs
        };

        // Build the heartbeat frame to be sent to the follower.
        let payload = AppendEntriesRequest {
            vote: self.session_id.vote,
            prev_log_id: req.prev_log_id,
            leader_commit: self.committed,
            entries: logs,
        };

        // Send the payload.
        tracing::debug!(
            payload=%payload.summary(),
            "start sending append_entries, timeout: {:?}",
            self.config.heartbeat_interval
        );

        let the_timeout = Duration::from_millis(self.config.heartbeat_interval);
        let res = timeout(the_timeout, self.network.send_append_entries(payload)).await;

        tracing::debug!("append_entries res: {:?}", res);

        let append_res = res.map_err(|_e| {
            let to = Timeout {
                action: RPCTypes::AppendEntries,
                id: self.session_id.vote.leader_id().voted_for().unwrap(),
                target: self.target,
                timeout: the_timeout,
            };
            RPCError::Timeout(to)
        })?;
        let append_resp = append_res?;

        tracing::debug!("append_entries resp: {:?}", append_resp);

        match append_resp {
            AppendEntriesResponse::Success => {
                self.update_matching(id, req.last_log_id);
                Ok(())
            }
            AppendEntriesResponse::HigherVote(vote) => {
                debug_assert!(
                    vote > self.session_id.vote,
                    "higher vote({}) should be greater than leader's vote({})",
                    vote,
                    self.session_id.vote,
                );
                tracing::debug!(%vote, "append entries failed. converting to follower");

                Err(ReplicationError::HigherVote(HigherVote {
                    higher: vote,
                    mine: self.session_id.vote,
                }))
            }
            AppendEntriesResponse::Conflict => {
                let conflict = req.prev_log_id;
                debug_assert!(conflict.is_some(), "prev_log_id=None never conflict");

                let conflict = conflict.unwrap();
                self.update_conflicting(id, conflict);

                Ok(())
            }
        }
    }

    fn update_conflicting(&mut self, id: u64, conflict: LogId<C::NodeId>) {
        tracing::debug!(
            target = display(self.target),
            id = display(id),
            conflict = display(&conflict),
            "update_conflicting"
        );

        let _ = self.tx_raft_core.send(RaftMsg::UpdateReplicationProgress {
            session_id: self.session_id,
            id,
            target: self.target,
            result: Ok(ReplicationResult::Conflict(conflict)),
        });
    }

    /// Update the `matched` and `max_possible_matched_index`, which both are for tracking
    /// follower replication(the left and right cursor in a bsearch).
    /// And also report the matched log id to RaftCore to commit an entry etc.
    #[tracing::instrument(level = "trace", skip(self))]
    fn update_matching(&mut self, id: u64, new_matching: Option<LogId<C::NodeId>>) {
        tracing::debug!(
            id = display(id),
            target = display(self.target),
            matching = debug(&new_matching),
            "update_matching"
        );

        debug_assert!(self.matching <= new_matching);

        if self.matching < new_matching {
            self.matching = new_matching;

            let _ = self.tx_raft_core.send(RaftMsg::UpdateReplicationProgress {
                session_id: self.session_id,
                id,
                target: self.target,
                result: Ok(ReplicationResult::Matching(new_matching)),
            });
        }
    }

    /// Receive and process events from RaftCore, until `next_action` is filled.
    ///
    /// It blocks until at least one event is received.
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn drain_events(&mut self) -> Result<(), ReplicationError<C::NodeId, C::Node>> {
        tracing::debug!("drain_events");

        let event = self.rx_repl.recv().await.ok_or(ReplicationError::Closed)?;
        self.process_event(event);

        self.try_drain_events().await?;

        // No action filled after all events are processed, fill in an action to send committed
        // index.
        if self.next_action.is_none() {
            let m = &self.matching;

            // empty message, just for syncing the committed index
            self.next_action = Some(Data {
                // id==0 will be ignored by RaftCore.
                id: 0,
                payload: Payload::Logs(LogIdRange {
                    prev_log_id: *m,
                    last_log_id: *m,
                }),
            });
        }

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip(self))]
    pub async fn try_drain_events(&mut self) -> Result<(), ReplicationError<C::NodeId, C::Node>> {
        tracing::debug!("try_drain_raft_rx");

        while self.next_action.is_none() {
            let maybe_res = self.rx_repl.recv().now_or_never();

            let recv_res = match maybe_res {
                None => {
                    // No more events in self.repl_rx
                    return Ok(());
                }
                Some(x) => x,
            };

            let event = recv_res.ok_or(ReplicationError::Closed)?;

            self.process_event(event);
        }

        Ok(())
    }

    #[tracing::instrument(level = "trace", skip_all)]
    pub fn process_event(&mut self, event: Replicate<C::NodeId, C::Node, S::SnapshotData>) {
        tracing::debug!(event=%event.summary(), "process_event");

        match event {
            Replicate::Committed(c) => {
                // RaftCore may send a committed equals to the initial value.
                debug_assert!(
                    c >= self.committed,
                    "expect new committed {} > self.committed {}",
                    c.summary(),
                    self.committed.summary()
                );

                self.committed = c;
            }
            Replicate::Heartbeat => {
                // Nothing to do. Heartbeat message is just for waking up replication to send
                // something: When all messages are drained,
                // - if self.next_action is None, it resend an empty AppendEntries request as
                //   heartbeat.
                //-  If self.next_action is not None, next_action will serve as a heartbeat.
            }
            Replicate::Data(d) => {
                debug_assert!(self.next_action.is_none(),);
                self.next_action = Some(d);
            }
        }
    }
}

/// Request to replicate a chunk of data, logs or snapshot.
///
/// It defines what data to send to a follower/learner and an id to identify who is sending this
/// data.
#[derive(Debug)]
pub(crate) struct Data<NID, N, SD>
where
    NID: NodeId,
    N: Node,
    SD: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    id: u64,
    payload: Payload<NID, N, SD>,
}

impl<NID, N, S> MessageSummary<Data<NID, N, S>> for Data<NID, N, S>
where
    NID: NodeId,
    N: Node,
    S: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    fn summary(&self) -> String {
        match &self.payload {
            Payload::Logs(log_id_range) => {
                format!("Logs{{id={}, {}}}", self.id, log_id_range)
            }
            Payload::Snapshot(snapshot) => {
                format!("Snapshot{{id={}, {}}}", self.id, snapshot.meta.summary())
            }
        }
    }
}

impl<NID, N, SD> Data<NID, N, SD>
where
    NID: NodeId,
    N: Node,
    SD: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    fn new_logs(id: u64, log_id_range: LogIdRange<NID>) -> Self {
        Self {
            id,
            payload: Payload::Logs(log_id_range),
        }
    }

    fn new_snapshot(id: u64, snapshot: Snapshot<NID, N, SD>) -> Self {
        Self {
            id,
            payload: Payload::Snapshot(snapshot),
        }
    }
}

/// The data to replication.
///
/// Either a series of logs or a snapshot.
pub(crate) enum Payload<NID, N, SD>
where
    NID: NodeId,
    N: Node,
    SD: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    Logs(LogIdRange<NID>),
    Snapshot(Snapshot<NID, N, SD>),
}

impl<NID, N, SD> Debug for Payload<NID, N, SD>
where
    NID: NodeId,
    N: Node,
    SD: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Logs(log_id_range) => {
                write!(f, "Logs({})", log_id_range)
            }
            Self::Snapshot(snapshot) => {
                write!(f, "Snapshot({:?})", snapshot.meta)
            }
        }
    }
}

/// Result of an replication action.
#[derive(Clone, Debug)]
pub(crate) enum ReplicationResult<NID: NodeId> {
    Matching(Option<LogId<NID>>),
    Conflict(LogId<NID>),
}

/// A replication request sent by RaftCore leader state to replication stream.
pub(crate) enum Replicate<NID, N, SD>
where
    NID: NodeId,
    N: Node,
    SD: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    /// Inform replication stream to forward the committed log id to followers/learners.
    Committed(Option<LogId<NID>>),

    /// Send an empty AppendEntries RPC as heartbeat.
    Heartbeat,

    /// Send a chunk of data, e.g., logs or snapshot.
    Data(Data<NID, N, SD>),
}

impl<NID, N, SD> Replicate<NID, N, SD>
where
    NID: NodeId,
    N: Node,
    SD: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    pub(crate) fn logs(id: u64, log_id_range: LogIdRange<NID>) -> Self {
        Self::Data(Data::new_logs(id, log_id_range))
    }

    pub(crate) fn snapshot(id: u64, snapshot: Snapshot<NID, N, SD>) -> Self {
        Self::Data(Data::new_snapshot(id, snapshot))
    }
}

impl<NID, N, S> MessageSummary<Replicate<NID, N, S>> for Replicate<NID, N, S>
where
    NID: NodeId,
    N: Node,
    S: AsyncRead + AsyncSeek + Send + Unpin + 'static,
{
    fn summary(&self) -> String {
        match self {
            Replicate::Committed(c) => {
                format!("Replicate::Committed: {:?}", c)
            }
            Replicate::Heartbeat => "Replicate::Heartbeat".to_string(),
            Replicate::Data(d) => {
                format!("Replicate::Data({})", d.summary())
            }
        }
    }
}

impl<C: RaftTypeConfig, N: RaftNetworkFactory<C>, S: RaftStorage<C>> ReplicationCore<C, N, S> {
    #[tracing::instrument(level = "trace", skip(self, snapshot))]
    async fn stream_snapshot(
        &mut self,
        id: u64,
        mut snapshot: Snapshot<C::NodeId, C::Node, S::SnapshotData>,
    ) -> Result<(), ReplicationError<C::NodeId, C::Node>> {
        tracing::debug!(id = display(id), snapshot = debug(&snapshot.meta), "stream_snapshot",);

        let err_x = || (ErrorSubject::Snapshot(snapshot.meta.signature()), ErrorVerb::Read);

        let mut offset = 0;
        let end = snapshot.snapshot.seek(SeekFrom::End(0)).await.sto_res(err_x)?;
        let mut buf = Vec::with_capacity(self.config.snapshot_max_chunk_size as usize);

        loop {
            // Build the RPC.
            snapshot.snapshot.seek(SeekFrom::Start(offset)).await.sto_res(err_x)?;
            let n_read = snapshot.snapshot.read_buf(&mut buf).await.sto_res(err_x)?;

            let done = (offset + n_read as u64) == end;
            let req = InstallSnapshotRequest {
                vote: self.session_id.vote,
                meta: snapshot.meta.clone(),
                offset,
                data: Vec::from(&buf[..n_read]),
                done,
            };
            buf.clear();

            // Send the RPC over to the target.
            tracing::debug!(
                snapshot_size = req.data.len(),
                req.offset,
                end,
                req.done,
                "sending snapshot chunk"
            );

            let snap_timeout = if done {
                self.config.install_snapshot_timeout()
            } else {
                self.config.send_snapshot_timeout()
            };

            let res = timeout(snap_timeout, self.network.send_install_snapshot(req)).await;

            let res = match res {
                Ok(outer_res) => match outer_res {
                    Ok(res) => res,
                    Err(err) => {
                        tracing::warn!(error=%err, "error sending InstallSnapshot RPC to target");

                        // Sleep a short time otherwise in test environment it is a dead-loop that
                        // never yields. Because network implementation does
                        // not yield.
                        sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                },
                Err(err) => {
                    tracing::warn!(error=%err, "timeout while sending InstallSnapshot RPC to target");

                    // Sleep a short time otherwise in test environment it is a dead-loop that never
                    // yields. Because network implementation does not yield.
                    sleep(Duration::from_millis(10)).await;
                    continue;
                }
            };

            // Handle response conditions.
            if res.vote > self.session_id.vote {
                return Err(ReplicationError::HigherVote(HigherVote {
                    higher: res.vote,
                    mine: self.session_id.vote,
                }));
            }

            // If we just sent the final chunk of the snapshot, then transition to lagging state.
            if done {
                tracing::debug!(
                    "done install snapshot: snapshot last_log_id: {:?}, matching: {}",
                    snapshot.meta.last_log_id,
                    self.matching.summary(),
                );

                self.update_matching(id, snapshot.meta.last_log_id);

                return Ok(());
            }

            // Everything is good, so update offset for sending the next chunk.
            offset += n_read as u64;

            // Check raft channel to ensure we are staying up-to-date, then loop.
            self.try_drain_events().await?;
        }
    }
}
