use std::io::SeekFrom;
use std::marker::PhantomData;

use tokio::io::AsyncSeek;
use tokio::io::AsyncSeekExt;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;

use crate::raft::InstallSnapshotRequest;
use crate::ErrorSubject;
use crate::ErrorVerb;
use crate::RaftTypeConfig;
use crate::SnapshotId;
use crate::StorageError;

/// The Raft node is streaming in a snapshot from the leader.
pub(crate) struct StreamingState<C: RaftTypeConfig, SD> {
    /// The offset of the last byte written to the snapshot.
    pub(crate) offset: u64,
    /// The ID of the snapshot being written.
    pub(crate) snapshot_id: SnapshotId,
    /// A handle to the snapshot writer.
    pub(crate) snapshot_data: Box<SD>,

    _p: PhantomData<C>,
}

impl<C: RaftTypeConfig, SD> StreamingState<C, SD>
where SD: AsyncSeek + AsyncWrite + Unpin
{
    pub(crate) fn new(snapshot_id: SnapshotId, snapshot_data: Box<SD>) -> Self {
        Self {
            offset: 0,
            snapshot_id,
            snapshot_data,
            _p: Default::default(),
        }
    }

    /// Receive a chunk of snapshot data.
    pub(crate) async fn receive(&mut self, req: InstallSnapshotRequest<C>) -> Result<bool, StorageError<C::NodeId>> {
        // TODO: check id?

        // Always seek to the target offset if not an exact match.
        if req.offset != self.offset {
            if let Err(err) = self.snapshot_data.as_mut().seek(SeekFrom::Start(req.offset)).await {
                return Err(StorageError::from_io_error(
                    ErrorSubject::Snapshot(req.meta.signature()),
                    ErrorVerb::Seek,
                    err,
                ));
            }
            self.offset = req.offset;
        }

        // Write the next segment & update offset.
        let res = self.snapshot_data.as_mut().write_all(&req.data).await;
        if let Err(err) = res {
            return Err(StorageError::from_io_error(
                ErrorSubject::Snapshot(req.meta.signature()),
                ErrorVerb::Write,
                err,
            ));
        }
        self.offset += req.data.len() as u64;
        Ok(req.done)
    }
}
