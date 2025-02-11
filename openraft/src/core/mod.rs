//! The `RaftCore` is a `Runtime` supporting the raft algorithm implementation `Engine`.
//!
//! It passes events from an application or timer or network to `Engine` to drive it to run.
//! Also it receives and execute `Command` emitted by `Engine` to apply raft state to underlying
//! storage or forward messages to other raft nodes.

mod install_snapshot;
mod raft_core;
mod replication_state;
mod server_state;
mod snapshot_state;
mod streaming_state;
mod tick;

pub use raft_core::RaftCore;
pub(crate) use replication_state::replication_lag;
pub use server_state::ServerState;
pub(crate) use snapshot_state::SnapshotResult;
pub(crate) use snapshot_state::SnapshotState;
pub(crate) use tick::Tick;
pub(crate) use tick::TickHandle;
