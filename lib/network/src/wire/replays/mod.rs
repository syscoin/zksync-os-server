//! Replay request and response payloads for the `zks` protocol.
//!
//! SYSCOIN: The fresh V32 lane retains only the test format and upstream production format.
//! The protocol version pins the replay record format used inside [`BlockReplays`]: production
//! `zks/5` uses [`v3`], while negotiation tests can use the deliberately lossy [`v0`].

pub mod v0;
pub mod v3;

mod impls;

use alloy::consensus::crypto::RecoveryError;
use alloy::primitives::{BlockNumber, Bytes};
use alloy_rlp::{Decodable, Encodable, Header, RlpDecodable, RlpEncodable};
use std::fmt::Debug;
use zksync_os_storage_api::ReplayRecord as StorageReplayRecord;

/// Opens a replay stream beginning at [`Self::starting_block`].
///
/// The peer keeps returning [`BlockReplays`] responses until the connection or its replay storage
/// closes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RlpEncodable)]
#[rlp(trailing)]
pub struct GetBlockReplays {
    /// The block number that the peer should start returning replay blocks from.
    pub starting_block: u64,
    /// Records for which DB keys should be overridden. Used only for debugging.
    pub record_overrides: Vec<RecordOverride>,
    /// Requested maximum number of consecutive records in each response.
    ///
    /// `None` defaults to one record. The main node clamps a supplied value to its supported range.
    pub max_blocks_per_message: Option<u64>,
}

// SYSCOIN: Bound untrusted replay-override structure before RLP decoding allocates one entry per
// item. RLPx limits wire bytes, but a compact list can otherwise amplify into much larger retained
// Vec and HashMap allocations for every connected peer.
pub(crate) const MAX_REPLAY_RECORD_OVERRIDES: usize = 1_024;
pub(crate) const MAX_REPLAY_OVERRIDE_DB_KEY_BYTES: usize = 32;
pub(crate) const MAX_REPLAY_OVERRIDE_PAYLOAD_BYTES: usize = 64 * 1024;

impl Decodable for GetBlockReplays {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let mut payload = Header::decode_bytes(buf, true)?;
        let starting_block = BlockNumber::decode(&mut payload)?;

        // Check the encoded list before decoding its elements. This bounds both a single large key
        // and structural amplification from many tiny entries without changing the wire format.
        let mut encoded_overrides = Header::decode_bytes(&mut payload, true)?;
        if encoded_overrides.len() > MAX_REPLAY_OVERRIDE_PAYLOAD_BYTES {
            return Err(alloy_rlp::Error::Custom(
                "replay override payload exceeds limit",
            ));
        }

        let mut record_overrides = Vec::new();
        while !encoded_overrides.is_empty() {
            if record_overrides.len() == MAX_REPLAY_RECORD_OVERRIDES {
                return Err(alloy_rlp::Error::Custom(
                    "replay override count exceeds limit",
                ));
            }
            let record_override = RecordOverride::decode(&mut encoded_overrides)?;
            if record_override.db_key.len() > MAX_REPLAY_OVERRIDE_DB_KEY_BYTES {
                return Err(alloy_rlp::Error::Custom(
                    "replay override db key exceeds limit",
                ));
            }
            record_overrides.push(record_override);
        }

        let max_blocks_per_message = match payload.first() {
            None => None,
            Some(&alloy_rlp::EMPTY_STRING_CODE) => {
                payload = &payload[1..];
                None
            }
            Some(_) => Some(u64::decode(&mut payload)?),
        };
        if !payload.is_empty() {
            return Err(alloy_rlp::Error::UnexpectedLength);
        }

        Ok(Self {
            starting_block,
            record_overrides,
            max_blocks_per_message,
        })
    }
}

/// Asks the main node to read one replay record from an explicit database key.
///
/// This lets an external node debug a reverted block whose record is no longer canonical.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
pub struct RecordOverride {
    /// Block number for which record should be pulled from a different DB key.
    pub block_number: BlockNumber,
    /// DB key to use when reading replay record.
    pub db_key: Bytes,
}

/// The response to [`GetBlockReplays`], containing one or more consecutive replay records.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
pub struct BlockReplays<T: WireReplayRecord> {
    pub records: Vec<T>,
}

impl<T: WireReplayRecord> BlockReplays<T> {
    pub fn new(records: Vec<StorageReplayRecord>) -> Self {
        let records = records.into_iter().map(T::from).collect();
        Self { records }
    }
}

/// Conversion boundary between storage replay records and immutable wire formats.
///
/// Each `zks` protocol version pins one implementation, while the sequencer and storage layers
/// continue to use [`StorageReplayRecord`].
pub trait WireReplayRecord:
    From<StorageReplayRecord>
    + TryInto<StorageReplayRecord, Error = RecoveryError>
    + Encodable
    + Decodable
    + Debug
    + Send
    + Sync
    + Unpin
{
    /// Returns the record's block number without converting the full payload.
    fn block_number(&self) -> BlockNumber;
}
