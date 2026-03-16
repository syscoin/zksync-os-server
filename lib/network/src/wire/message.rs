//! Defines structs/enums for messages and request-response pairs used in zks wire protocol.
//! Handles compatibility with [`ZksVersion`].
//!
//! Examples include creating, encoding, and decoding protocol messages.

use crate::version::AnyZksProtocolVersion;
use crate::wire::BlockReplays;
use crate::wire::replays::{GetBlockReplays, GetBlockReplaysV2, RecordOverride};
use alloy::primitives::BlockNumber;
use alloy::primitives::bytes::{Buf, BufMut, BytesMut};
use alloy_rlp::{Decodable, Encodable, Error as RlpError};
use reth_eth_wire::protocol::Protocol;
use reth_network::types::Capability;
use std::fmt::Debug;
use zksync_os_storage_api::ReplayRecord;

pub const ZKS_PROTOCOL: &str = "zks";

/// Represents a message in the zks wire protocol.
///
/// Let's call main node MN and external node EN. As there can only be one MN, the connection can
/// be either EN<->MN or EN<->EN.
///
/// In an EN<->MN connection:
///  * EN MUST send exactly one [`GetBlockReplays`] request at the start of connection (v1).
///  * MN MUST NOT send any [`GetBlockReplays`] or [`GetBlockReplaysV2`] requests.
///  * **v1 (infinite streaming)**: EN sends one [`GetBlockReplays`] (0x00) and MN responds with
///    an indefinite stream of [`BlockReplays`] messages.
///  * **v2 (on-demand)**: EN sends [`GetBlockReplaysV2`] (0x02) with `record_count = N`; MN
///    responds with exactly N [`BlockReplays`] messages then stops. EN then sends a new
///    [`GetBlockReplaysV2`] to request the next batch.
///
/// In an EN<->EN connection:
///  * Both ENs MUST NOT send or receive any messages from each other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ZksMessage<P: AnyZksProtocolVersion> {
    /// Represents a `GetBlockReplays` streaming request (v0/v1, message ID 0x00).
    GetBlockReplays(GetBlockReplays),
    /// Represents a `BlockReplays` response (one of many, message ID 0x01).
    BlockReplays(BlockReplays<P::Record>),
    /// Represents a `GetBlockReplaysV2` on-demand request (v2, message ID 0x02).
    GetBlockReplaysV2(GetBlockReplaysV2),
}

impl<P: AnyZksProtocolVersion> ZksMessage<P> {
    /// Returns the capability for the zks protocol.
    pub const fn capability() -> Capability {
        Capability::new_static(ZKS_PROTOCOL, P::VERSION as usize)
    }

    /// Returns the protocol for the zks protocol.
    pub const fn protocol() -> Protocol {
        Protocol::new(Self::capability(), P::VERSION.message_count())
    }

    /// Returns the message's ID.
    pub const fn message_id(&self) -> ZksMessageId {
        match self {
            ZksMessage::GetBlockReplays(_) => ZksMessageId::GetBlockReplays,
            ZksMessage::BlockReplays(_) => ZksMessageId::BlockReplays,
            ZksMessage::GetBlockReplaysV2(_) => ZksMessageId::GetBlockReplaysV2,
        }
    }

    /// Construct a v0/v1-style indefinite-stream request.
    pub fn get_block_replays(
        starting_block: BlockNumber,
        record_overrides: Vec<RecordOverride>,
    ) -> Self {
        Self::GetBlockReplays(GetBlockReplays {
            starting_block,
            record_overrides,
        })
    }

    /// Construct a v2-style bounded-batch request.
    pub fn get_block_replays_v2(
        starting_block: BlockNumber,
        record_count: u64,
        record_overrides: Vec<RecordOverride>,
    ) -> Self {
        Self::GetBlockReplaysV2(GetBlockReplaysV2 {
            starting_block,
            record_count,
            record_overrides,
        })
    }

    pub fn block_replays(records: Vec<ReplayRecord>) -> Self {
        Self::BlockReplays(BlockReplays::new(records))
    }

    /// Return RLP encoded message.
    pub fn encoded(&self) -> BytesMut {
        let mut buf = BytesMut::with_capacity(self.length());
        self.encode(&mut buf);
        buf
    }

    /// Decodes a `ZksMessage` from the given message buffer.
    pub fn decode_message(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let message_type = ZksMessageId::decode(buf)?;
        Ok(match message_type {
            ZksMessageId::GetBlockReplays => Self::GetBlockReplays(GetBlockReplays::decode(buf)?),
            ZksMessageId::BlockReplays => {
                Self::BlockReplays(BlockReplays::<P::Record>::decode(buf)?)
            }
            ZksMessageId::GetBlockReplaysV2 => {
                Self::GetBlockReplaysV2(GetBlockReplaysV2::decode(buf)?)
            }
        })
    }
}

impl<P: AnyZksProtocolVersion> Encodable for ZksMessage<P> {
    fn encode(&self, out: &mut dyn BufMut) {
        self.message_id().encode(out);
        match self {
            ZksMessage::GetBlockReplays(message) => message.encode(out),
            ZksMessage::BlockReplays(message) => message.encode(out),
            ZksMessage::GetBlockReplaysV2(message) => message.encode(out),
        }
    }

    fn length(&self) -> usize {
        self.message_id().length()
            + match self {
                ZksMessage::GetBlockReplays(message) => message.length(),
                ZksMessage::BlockReplays(message) => message.length(),
                ZksMessage::GetBlockReplaysV2(message) => message.length(),
            }
    }
}

/// Represents message IDs for zks protocol messages.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZksMessageId {
    /// Get block replays message (v0/v1 indefinite stream).
    GetBlockReplays = 0x00,
    /// Block replays message.
    BlockReplays = 0x01,
    /// Get block replays v2 message (v2 on-demand batch).
    GetBlockReplaysV2 = 0x02,
}

impl ZksMessageId {
    /// Returns the corresponding `u8` value for a `ZksMessageId`.
    pub const fn as_u8(&self) -> u8 {
        *self as u8
    }
}

impl Encodable for ZksMessageId {
    fn encode(&self, out: &mut dyn BufMut) {
        out.put_u8(self.as_u8());
    }
    fn length(&self) -> usize {
        1
    }
}

impl Decodable for ZksMessageId {
    fn decode(buf: &mut &[u8]) -> alloy::rlp::Result<Self> {
        let byte = buf.first().ok_or(alloy_rlp::Error::InputTooShort)?;
        let id = ZksMessageId::try_from(*byte).map_err(RlpError::Custom)?;
        buf.advance(1);
        Ok(id)
    }
}

impl TryFrom<u8> for ZksMessageId {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::GetBlockReplays),
            0x01 => Ok(Self::BlockReplays),
            0x02 => Ok(Self::GetBlockReplaysV2),
            _ => Err("unrecognized zks message id"),
        }
    }
}
