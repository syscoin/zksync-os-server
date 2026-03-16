//! Support for representing the version of the `zks` protocol

use crate::wire::message::{ZksMessage, ZksMessageId};
use crate::wire::replays::{
    GetBlockReplays, GetBlockReplaysV2, RecordOverride, WireReplayRecord, v0, v1, v2,
};
use alloy::primitives::BlockNumber;
use alloy::primitives::bytes::{BufMut, BytesMut};
use alloy::rlp::{Decodable, Encodable, Error as RlpError};
use futures::FutureExt;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use zksync_os_storage_api::{ReadReplay, ReadReplayExt, ReplayRecord};

/// How many records v2 requests per batch.
const RECORDS_PER_REQUEST_V2: u64 = 64;

/// Any protocol version along with its pinned wire formats.
pub trait AnyZksProtocolVersion: Debug + Send + Sync + Unpin + Clone + 'static {
    /// Wire format for replay record.
    type Record: WireReplayRecord;

    /// Version number matching this protocol version.
    const VERSION: ZksVersion;

    /// Background task that drives the **external-node** side of a connection.
    ///
    /// Sends a [`GetBlockReplays`] request immediately, then forwards each received
    /// [`BlockReplays`] record to the local sequencer via `replay_sender` and advances
    /// `starting_block`.
    ///
    /// The default implementation is the v1 infinite-streaming behaviour.
    fn run_en_connection(
        conn: impl Stream<Item = ZksMessage<Self>> + Unpin + Send + 'static,
        outbound_tx: mpsc::Sender<BytesMut>,
        starting_block: Arc<RwLock<BlockNumber>>,
        record_overrides: Vec<RecordOverride>,
        replay_sender: mpsc::Sender<ReplayRecord>,
    ) -> BoxFuture<'static, ()>
    where
        Self: Sized,
    {
        async move {
            let next_block = *starting_block.read().unwrap();
            tracing::info!(next_block, "requesting block replays from main node");
            let msg = ZksMessage::<Self>::get_block_replays(next_block, record_overrides);
            if outbound_tx.send(msg.encoded()).await.is_err() {
                return;
            }

            let mut conn = conn;
            while let Some(msg) = conn.next().await {
                let response = match msg {
                    ZksMessage::GetBlockReplays(_) | ZksMessage::GetBlockReplaysV2(_) => {
                        tracing::info!(
                            "ignoring request as local node is also waiting for records"
                        );
                        continue;
                    }
                    ZksMessage::BlockReplays(response) => response,
                };
                // todo: logic below relies on there being one record per message
                //       we can (and should) adapt it to handle multiple records in the future
                assert_eq!(
                    response.records.len(),
                    1,
                    "only 1 record per message is supported right now"
                );
                let record = response.records.into_iter().next().unwrap();
                let block_number = record.block_number();
                tracing::debug!(block_number, "received block replay");
                let record: ReplayRecord = match record.try_into() {
                    Ok(record) => record,
                    Err(error) => {
                        tracing::info!(%error, "failed to recover replay block");
                        break;
                    }
                };

                let expected_next_block = *starting_block.read().unwrap();
                assert_eq!(block_number, expected_next_block);

                if replay_sender.send(record).await.is_err() {
                    tracing::trace!("network replay channel is closed");
                    break;
                }
                // Only advance after the record is successfully delivered, so a reconnect
                // does not skip a block if the channel send was the last thing to fail.
                *starting_block.write().unwrap() += 1;
            }
        }
        .boxed()
    }

    /// Background task that drives the **main-node** side of a connection.
    ///
    /// Waits for a [`GetBlockReplays`] request from the EN, then streams replay records from
    /// storage to the EN indefinitely.
    ///
    /// The default implementation is the v1 infinite-streaming behaviour.
    fn run_mn_connection<Replay: ReadReplay + Clone + Send + 'static>(
        conn: impl Stream<Item = ZksMessage<Self>> + Unpin + Send + 'static,
        outbound_tx: mpsc::Sender<BytesMut>,
        replay: Replay,
    ) -> BoxFuture<'static, ()>
    where
        Self: Sized,
    {
        async move {
            let mut conn = conn;
            // Receive the single GetBlockReplays request for this connection.
            let request = match conn.next().await {
                Some(ZksMessage::GetBlockReplays(request)) => request,
                Some(other) => {
                    tracing::info!(?other, "received unexpected message; terminating");
                    return;
                }
                None => return,
            };

            // Stream records to the EN indefinitely.
            let mut stream = replay
                .clone()
                .stream_from_forever(request.starting_block, HashMap::new());
            loop {
                tokio::select! {
                    // Biased because first branch always leads to early return. Makes sense to
                    // check it first.
                    biased;

                    msg = conn.next() => {
                        // No messages are expected from the peer after GetBlockReplays.
                        match msg {
                            Some(msg) => tracing::info!(?msg, "received unexpected message from peer; terminating"),
                            None => tracing::info!("peer connection closed; terminating"),
                        }
                        return;
                    }
                    record = stream.next() => {
                        let Some(record) = record else {
                            // stream_from_forever only ends if storage closes.
                            tracing::info!("replay stream closed; terminating");
                            return;
                        };
                        let encoded = ZksMessage::<Self>::block_replays(vec![record]).encoded();
                        if outbound_tx.send(encoded).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
        .boxed()
    }
}

/// Protocol version 0 is very bare-bones and used purely for testing.
#[derive(Debug, Clone)]
pub struct ZksProtocolV0;

impl AnyZksProtocolVersion for ZksProtocolV0 {
    type Record = v0::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks0;
}

/// Protocol version 1 is the initial implementation that supports `GetBlockReplays` and `BlockReplays`
/// message types.
#[derive(Debug, Clone)]
pub struct ZksProtocolV1;

impl AnyZksProtocolVersion for ZksProtocolV1 {
    type Record = v1::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks1;
}

/// Protocol version 2 adds on-demand fetching: ENs request records in fixed-size batches via the
/// new [`GetBlockReplaysV2`] message (ID 0x02) instead of receiving an indefinite stream.
#[derive(Debug, Clone)]
pub struct ZksProtocolV2;

impl AnyZksProtocolVersion for ZksProtocolV2 {
    type Record = v2::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks2;

    fn run_en_connection(
        conn: impl Stream<Item = ZksMessage<Self>> + Unpin + Send + 'static,
        outbound_tx: mpsc::Sender<BytesMut>,
        starting_block: Arc<RwLock<BlockNumber>>,
        record_overrides: Vec<RecordOverride>,
        replay_sender: mpsc::Sender<ReplayRecord>,
    ) -> BoxFuture<'static, ()>
    where
        Self: Sized,
    {
        async move {
            // The initial request carries the provided overrides; subsequent re-requests use none.
            let mut overrides = record_overrides;
            let mut conn = conn;

            loop {
                let next_block = *starting_block.read().unwrap();
                tracing::info!(
                    next_block,
                    record_count = RECORDS_PER_REQUEST_V2,
                    "requesting block replays from main node"
                );
                let msg = ZksMessage::<Self>::get_block_replays_v2(
                    next_block,
                    RECORDS_PER_REQUEST_V2,
                    std::mem::take(&mut overrides),
                );
                if outbound_tx.send(msg.encoded()).await.is_err() {
                    return;
                }

                let mut received = 0u64;
                loop {
                    let msg = match conn.next().await {
                        Some(msg) => msg,
                        None => return,
                    };
                    let response = match msg {
                        ZksMessage::GetBlockReplays(_) | ZksMessage::GetBlockReplaysV2(_) => {
                            tracing::info!(
                                "ignoring request as local node is also waiting for records"
                            );
                            continue;
                        }
                        ZksMessage::BlockReplays(response) => response,
                    };
                    // todo: logic below relies on there being one record per message
                    //       we can (and should) adapt it to handle multiple records in the future
                    assert_eq!(
                        response.records.len(),
                        1,
                        "only 1 record per message is supported right now"
                    );
                    let record = response.records.into_iter().next().unwrap();
                    let block_number = record.block_number();
                    tracing::debug!(block_number, "received block replay");
                    let record: ReplayRecord = match record.try_into() {
                        Ok(record) => record,
                        Err(error) => {
                            tracing::info!(%error, "failed to recover replay block");
                            return;
                        }
                    };

                    let expected_next_block = *starting_block.read().unwrap();
                    assert_eq!(block_number, expected_next_block);

                    if replay_sender.send(record).await.is_err() {
                        tracing::trace!("network replay channel is closed");
                        return;
                    }
                    // Only advance after the record is successfully delivered, so a reconnect
                    // does not skip a block if the channel send was the last thing to fail.
                    *starting_block.write().unwrap() += 1;

                    received += 1;
                    if received >= RECORDS_PER_REQUEST_V2 {
                        break;
                    }
                }
                // Batch fully received — loop back to request the next batch.
            }
        }
        .boxed()
    }

    fn run_mn_connection<Replay: ReadReplay + Clone + Send + 'static>(
        conn: impl Stream<Item = ZksMessage<Self>> + Unpin + Send + 'static,
        outbound_tx: mpsc::Sender<BytesMut>,
        replay: Replay,
    ) -> BoxFuture<'static, ()>
    where
        Self: Sized,
    {
        async move {
            let mut conn = conn;
            loop {
                // Receive the next GetBlockReplaysV2 request.
                let request = match conn.next().await {
                    Some(ZksMessage::GetBlockReplaysV2(request)) => request,
                    Some(other) => {
                        tracing::info!(?other, "received unexpected message; terminating");
                        return;
                    }
                    None => return,
                };

                let mut stream = replay
                    .clone()
                    .stream_from_forever(request.starting_block, HashMap::new());
                let mut sent = 0u64;

                loop {
                    tokio::select! {
                        biased;

                        msg = conn.next() => {
                            // No messages are expected while we are serving this batch.
                            match msg {
                                Some(msg) => tracing::info!(?msg, "received unexpected message from peer; terminating"),
                                None => tracing::info!("peer connection closed; terminating"),
                            }
                            return;
                        }
                        record = stream.next() => {
                            let Some(record) = record else {
                                tracing::info!("replay stream closed; terminating");
                                return;
                            };
                            let encoded = ZksMessage::<Self>::block_replays(vec![record]).encoded();
                            if outbound_tx.send(encoded).await.is_err() {
                                return;
                            }
                            sent += 1;
                            if sent >= request.record_count {
                                break;
                            }
                        }
                    }
                }

                // Batch served — loop back to wait for the next GetBlockReplaysV2.
                tracing::debug!("finished serving batch of records; waiting for next request");
            }
        }
        .boxed()
    }
}

/// Error thrown when failed to parse a valid [`ZksVersion`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Unknown zks protocol version: {0}")]
pub struct ParseVersionError(String);

/// The `zks` protocol version.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZksVersion {
    /// The `zks` protocol version 0. Only used for testing.
    Zks0 = 0,
    /// The `zks` protocol version 1.
    Zks1 = 1,
    /// The `zks` protocol version 2.
    Zks2 = 2,
}

impl ZksVersion {
    /// The latest known zks version
    pub const LATEST: Self = Self::Zks1;

    /// All known zks versions
    pub const ALL_VERSIONS: &'static [Self] = &[Self::Zks0, Self::Zks1];

    /// Returns the max message id for the given version.
    const fn max_message_id(&self) -> u8 {
        match self {
            ZksVersion::Zks0 => ZksMessageId::BlockReplays as u8,
            ZksVersion::Zks1 => ZksMessageId::BlockReplays as u8,
            // v2 adds GetBlockReplaysV2 (0x02) as a separate message type.
            ZksVersion::Zks2 => ZksMessageId::GetBlockReplaysV2 as u8,
        }
    }

    /// Returns the total number of message types for the given version.
    pub(crate) const fn message_count(&self) -> u8 {
        self.max_message_id() + 1
    }
}

/// RLP encodes `ZksVersion` as a single byte.
impl Encodable for ZksVersion {
    fn encode(&self, out: &mut dyn BufMut) {
        (*self as u8).encode(out)
    }

    fn length(&self) -> usize {
        (*self as u8).length()
    }
}

/// RLP decodes a single byte into `ZksVersion`.
/// Returns error if byte is not a valid version.
impl Decodable for ZksVersion {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let version = u8::decode(buf)?;
        Self::try_from(version).map_err(|_| RlpError::Custom("invalid zks version"))
    }
}

/// Allow for converting from a u8 to an `ZksVersion`.
///
/// # Example
/// ```
/// use zksync_os_network::version::ZksVersion;
///
/// let version = ZksVersion::try_from(1).unwrap();
/// assert_eq!(version, ZksVersion::Zks1);
/// ```
impl TryFrom<u8> for ZksVersion {
    type Error = ParseVersionError;

    #[inline]
    fn try_from(u: u8) -> Result<Self, Self::Error> {
        match u {
            0 => Ok(Self::Zks0),
            1 => Ok(Self::Zks1),
            2 => Ok(Self::Zks2),
            _ => Err(ParseVersionError(u.to_string())),
        }
    }
}

impl From<ZksVersion> for u8 {
    #[inline]
    fn from(v: ZksVersion) -> Self {
        v as Self
    }
}

impl From<ZksVersion> for &'static str {
    #[inline]
    fn from(v: ZksVersion) -> &'static str {
        match v {
            ZksVersion::Zks0 => "0",
            ZksVersion::Zks1 => "1",
            ZksVersion::Zks2 => "2",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ZksVersion;
    use alloy::primitives::bytes::BytesMut;
    use alloy_rlp::{Decodable, Encodable, Error as RlpError};

    #[test]
    fn test_zks_version_rlp_encode() {
        // Version 0 is purposefully left out as it encodes to 0x80 (prefix for 0-length string)
        let versions = [ZksVersion::Zks1, ZksVersion::Zks2];

        for version in versions {
            let mut encoded = BytesMut::new();
            version.encode(&mut encoded);

            assert_eq!(encoded.len(), 1);
            assert_eq!(encoded[0], version as u8);
        }
    }

    #[test]
    fn test_zks_version_rlp_decode() {
        let test_cases = [
            (0_u8, Ok(ZksVersion::Zks0)),
            (1_u8, Ok(ZksVersion::Zks1)),
            (2_u8, Ok(ZksVersion::Zks2)),
            (3_u8, Err(RlpError::Custom("invalid zks version"))),
        ];

        for (input, expected) in test_cases {
            let mut encoded = BytesMut::new();
            input.encode(&mut encoded);

            let mut slice = encoded.as_ref();
            let result = ZksVersion::decode(&mut slice);
            assert_eq!(result, expected);
        }
    }
}
