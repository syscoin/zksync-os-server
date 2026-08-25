//! Definitions for `zks_2fa` wire-protocol messages and encode / decode helpers.
//!
//! The `zks_2fa` subprotocol carries verifier authentication and batch-verification request /
//! response traffic that previously lived inside `zks/3` and `zks/4`. It reuses the same message
//! payload types (see [`crate::wire::auth`] and [`crate::wire::verification`]) but renumbers the
//! message ids starting from `0x00` because it is a fresh, single-version protocol.

use crate::wire::auth::{VerifierAuth, VerifierChallenge, VerifierRoleRequest};
use crate::wire::verification::{VerifyBatch, VerifyBatchResult};
use alloy::primitives::{
    B256,
    bytes::{Buf, BufMut, BytesMut},
};
use alloy_rlp::{Decodable, Encodable, Error as RlpError};
use reth_eth_wire::protocol::Protocol;
use reth_network::types::Capability;

pub const ZKS_2FA_PROTOCOL: &str = "zks_2fa";
// SYSCOIN: zks_2fa/1 is unreleased pre-mainnet; its first released semantics will use the secure
// V2 chain-and-peer-bound auth transcript. Keep capability v1 rather than inventing a compatibility
// lane for draft behavior that was never deployed.
pub(crate) const ZKS_2FA_PROTOCOL_VERSION: usize = 1;
pub(crate) const ZKS_2FA_MESSAGE_COUNT: u8 = 5;

// SYSCOIN: Reject each raw protocol frame by variant before any RLP decoder can allocate a peer-
// declared Bytes/String payload. Control/auth/result frames remain tiny; VerifyBatch allows ample
// headroom over the canonical V32 ABI (32 operator hashes, 32 compact DA refs, and stored-batch
// data) without admitting an unbounded commit-data allocation.
const MAX_VERIFIER_ROLE_REQUEST_FRAME_BYTES: usize = 8;
const MAX_VERIFIER_CHALLENGE_FRAME_BYTES: usize = 64;
const MAX_VERIFIER_AUTH_FRAME_BYTES: usize = 128;
const MAX_VERIFY_BATCH_FRAME_BYTES: usize = 128 * 1024;
const MAX_VERIFY_BATCH_RESULT_FRAME_BYTES: usize = 512;

/// A `zks_2fa` wire-protocol message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Zks2faMessage {
    /// External node requests verifier role for the current session.
    VerifierRoleRequest(VerifierRoleRequest),
    /// Main-node provides verifier challenge.
    VerifierChallenge(VerifierChallenge),
    /// External node authentication response proving control of the verifier signing key.
    VerifierAuth(VerifierAuth),
    /// Main node requests an external-node verifier to validate and sign a batch.
    VerifyBatch(VerifyBatch),
    /// External-node verifier responds to a [`VerifyBatch`] request with approval or refusal.
    VerifyBatchResult(VerifyBatchResult),
}

impl Zks2faMessage {
    /// Returns the capability for the `zks_2fa` protocol.
    pub const fn capability() -> Capability {
        Capability::new_static(ZKS_2FA_PROTOCOL, ZKS_2FA_PROTOCOL_VERSION)
    }

    /// Returns the protocol for the `zks_2fa` protocol.
    pub const fn protocol() -> Protocol {
        Protocol::new(Self::capability(), ZKS_2FA_MESSAGE_COUNT)
    }

    /// Returns the message's ID.
    pub const fn message_id(&self) -> Zks2faMessageId {
        match self {
            Zks2faMessage::VerifierRoleRequest(_) => Zks2faMessageId::VerifierRoleRequest,
            Zks2faMessage::VerifierChallenge(_) => Zks2faMessageId::VerifierChallenge,
            Zks2faMessage::VerifierAuth(_) => Zks2faMessageId::VerifierAuth,
            Zks2faMessage::VerifyBatch(_) => Zks2faMessageId::VerifyBatch,
            Zks2faMessage::VerifyBatchResult(_) => Zks2faMessageId::VerifyBatchResult,
        }
    }

    pub fn verifier_role_request() -> Self {
        Self::VerifierRoleRequest(VerifierRoleRequest {})
    }

    pub fn verifier_challenge(nonce: B256) -> Self {
        Self::VerifierChallenge(VerifierChallenge { nonce })
    }

    /// Return RLP encoded message.
    pub fn encoded(&self) -> BytesMut {
        let mut buf = BytesMut::with_capacity(self.length());
        self.encode(&mut buf);
        buf
    }

    /// Decodes a `Zks2faMessage` from the given message buffer.
    pub fn decode_message(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        // SYSCOIN: Read only the fixed one-byte discriminant before applying the variant's raw
        // frame cap, so an oversized peer-controlled Bytes/String is never handed to its decoder.
        let frame_len = buf.len();
        let message_type = Zks2faMessageId::decode(buf)?;
        if frame_len > message_type.max_frame_bytes() {
            return Err(RlpError::Custom(
                "zks_2fa frame exceeds message-specific size limit",
            ));
        }
        let message = match message_type {
            Zks2faMessageId::VerifierRoleRequest => {
                Self::VerifierRoleRequest(VerifierRoleRequest::decode(buf)?)
            }
            Zks2faMessageId::VerifierChallenge => {
                Self::VerifierChallenge(VerifierChallenge::decode(buf)?)
            }
            Zks2faMessageId::VerifierAuth => Self::VerifierAuth(VerifierAuth::decode(buf)?),
            Zks2faMessageId::VerifyBatch => Self::VerifyBatch(VerifyBatch::decode(buf)?),
            Zks2faMessageId::VerifyBatchResult => {
                Self::VerifyBatchResult(VerifyBatchResult::decode(buf)?)
            }
        };
        // SYSCOIN: Each RLPx frame contains exactly one canonical message; ignored suffixes could
        // otherwise create transcript ambiguity or smuggle unbounded data past payload validation.
        if !buf.is_empty() {
            return Err(RlpError::Custom("trailing bytes in zks_2fa frame"));
        }
        Ok(message)
    }
}

impl Encodable for Zks2faMessage {
    fn encode(&self, out: &mut dyn BufMut) {
        self.message_id().encode(out);
        match self {
            Zks2faMessage::VerifierRoleRequest(message) => message.encode(out),
            Zks2faMessage::VerifierChallenge(message) => message.encode(out),
            Zks2faMessage::VerifierAuth(message) => message.encode(out),
            Zks2faMessage::VerifyBatch(message) => message.encode(out),
            Zks2faMessage::VerifyBatchResult(message) => message.encode(out),
        }
    }

    fn length(&self) -> usize {
        self.message_id().length()
            + match self {
                Zks2faMessage::VerifierRoleRequest(message) => message.length(),
                Zks2faMessage::VerifierChallenge(message) => message.length(),
                Zks2faMessage::VerifierAuth(message) => message.length(),
                Zks2faMessage::VerifyBatch(message) => message.length(),
                Zks2faMessage::VerifyBatchResult(message) => message.length(),
            }
    }
}

/// Represents message IDs for `zks_2fa` protocol messages.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zks2faMessageId {
    /// Request verifier role.
    VerifierRoleRequest = 0x00,
    /// Verifier challenge message.
    VerifierChallenge = 0x01,
    /// Verifier auth message.
    VerifierAuth = 0x02,
    /// Batch verification request.
    VerifyBatch = 0x03,
    /// Batch verification response.
    VerifyBatchResult = 0x04,
}

impl Zks2faMessageId {
    /// Returns the corresponding `u8` value for a `Zks2faMessageId`.
    pub const fn as_u8(&self) -> u8 {
        *self as u8
    }

    // SYSCOIN: Variant lookup occurs immediately after the single-byte message ID and before any
    // payload decoder, keeping attacker-controlled allocation bounded by the narrowest safe cap.
    const fn max_frame_bytes(self) -> usize {
        match self {
            Self::VerifierRoleRequest => MAX_VERIFIER_ROLE_REQUEST_FRAME_BYTES,
            Self::VerifierChallenge => MAX_VERIFIER_CHALLENGE_FRAME_BYTES,
            Self::VerifierAuth => MAX_VERIFIER_AUTH_FRAME_BYTES,
            Self::VerifyBatch => MAX_VERIFY_BATCH_FRAME_BYTES,
            Self::VerifyBatchResult => MAX_VERIFY_BATCH_RESULT_FRAME_BYTES,
        }
    }
}

impl Encodable for Zks2faMessageId {
    fn encode(&self, out: &mut dyn BufMut) {
        out.put_u8(self.as_u8());
    }
    fn length(&self) -> usize {
        1
    }
}

impl Decodable for Zks2faMessageId {
    fn decode(buf: &mut &[u8]) -> alloy::rlp::Result<Self> {
        let byte = buf.first().ok_or(alloy_rlp::Error::InputTooShort)?;
        let id = Zks2faMessageId::try_from(*byte).map_err(RlpError::Custom)?;
        buf.advance(1);
        Ok(id)
    }
}

impl TryFrom<u8> for Zks2faMessageId {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::VerifierRoleRequest),
            0x01 => Ok(Self::VerifierChallenge),
            0x02 => Ok(Self::VerifierAuth),
            0x03 => Ok(Self::VerifyBatch),
            0x04 => Ok(Self::VerifyBatchResult),
            _ => Err("unrecognized zks_2fa message id"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_VERIFIER_AUTH_FRAME_BYTES, MAX_VERIFIER_CHALLENGE_FRAME_BYTES,
        MAX_VERIFIER_ROLE_REQUEST_FRAME_BYTES, MAX_VERIFY_BATCH_FRAME_BYTES,
        MAX_VERIFY_BATCH_RESULT_FRAME_BYTES, Zks2faMessage, Zks2faMessageId,
    };
    use crate::wire::auth::{VerifierAuth, VerifierChallenge, VerifierRoleRequest};
    use crate::wire::verification::{VerifyBatch, VerifyBatchOutcome, VerifyBatchResult};
    use alloy::primitives::{B256, Bytes, U256};
    use alloy::sol_types::SolValue;
    use zksync_os_batch_types::{SYSCOIN_DA_MAX_BLOBS_PER_BATCH, SYSCOIN_DA_MAX_REFS_PER_BATCH};
    use zksync_os_contract_interface::{IExecutor, L2DACommitmentScheme};

    #[test]
    fn round_trips_all_messages() {
        let messages = [
            Zks2faMessage::VerifierRoleRequest(VerifierRoleRequest {}),
            Zks2faMessage::VerifierChallenge(VerifierChallenge {
                nonce: B256::repeat_byte(0x11),
            }),
            Zks2faMessage::VerifierAuth(VerifierAuth {
                signature: Bytes::from(vec![7u8; 65]),
            }),
            Zks2faMessage::VerifyBatch(VerifyBatch {
                request_id: 41,
                batch_number: 7,
                first_block_number: 100,
                last_block_number: 120,
                pubdata_mode: 0,
                commit_data: Bytes::from_static(b"commit"),
                prev_commit_data: Bytes::from_static(b"prev"),
                execution_protocol_version: 32,
            }),
            Zks2faMessage::VerifyBatchResult(VerifyBatchResult {
                request_id: 41,
                batch_number: 7,
                result: VerifyBatchOutcome::Approved(Bytes::from(vec![9u8; 65])),
            }),
        ];

        for message in messages {
            let encoded = message.encoded();
            let mut slice = encoded.as_ref();
            let decoded = Zks2faMessage::decode_message(&mut slice).unwrap();
            assert_eq!(decoded, message);
            assert_eq!(decoded.encoded(), encoded);
            assert!(slice.is_empty());
        }
    }

    #[test]
    fn rejects_unknown_message_id() {
        let err = Zks2faMessage::decode_message(&mut [0x05u8].as_ref()).unwrap_err();
        assert_eq!(
            err,
            alloy_rlp::Error::Custom("unrecognized zks_2fa message id")
        );
    }

    // SYSCOIN: Canonical payload decoding must consume the entire RLPx frame.
    #[test]
    fn rejects_trailing_frame_bytes() {
        let mut encoded = Zks2faMessage::verifier_role_request().encoded();
        encoded.extend_from_slice(&[0_u8]);

        assert_eq!(
            Zks2faMessage::decode_message(&mut encoded.as_ref()).unwrap_err(),
            alloy_rlp::Error::Custom("trailing bytes in zks_2fa frame")
        );
    }

    // SYSCOIN: Every wire variant enforces its own pre-decode raw-frame allocation boundary.
    #[test]
    fn rejects_each_variant_above_its_raw_frame_cap() {
        for (message_id, max_frame_bytes) in [
            (
                Zks2faMessageId::VerifierRoleRequest,
                MAX_VERIFIER_ROLE_REQUEST_FRAME_BYTES,
            ),
            (
                Zks2faMessageId::VerifierChallenge,
                MAX_VERIFIER_CHALLENGE_FRAME_BYTES,
            ),
            (Zks2faMessageId::VerifierAuth, MAX_VERIFIER_AUTH_FRAME_BYTES),
            (Zks2faMessageId::VerifyBatch, MAX_VERIFY_BATCH_FRAME_BYTES),
            (
                Zks2faMessageId::VerifyBatchResult,
                MAX_VERIFY_BATCH_RESULT_FRAME_BYTES,
            ),
        ] {
            let mut frame = vec![0_u8; max_frame_bytes + 1];
            frame[0] = message_id.as_u8();
            assert_eq!(
                Zks2faMessage::decode_message(&mut frame.as_slice()).unwrap_err(),
                alloy_rlp::Error::Custom("zks_2fa frame exceeds message-specific size limit"),
                "message {message_id:?} was not rejected at its raw cap",
            );
        }
    }

    #[test]
    fn verify_batch_cap_covers_exact_canonical_v32_maximum_with_headroom() {
        // SYSCOIN: Generate the actual pinned contract structs at the production Syscoin limits.
        // One hash per edge-ref message maximizes ABI fragmentation and therefore wire size.
        let edge_ref_message = (
            U256::from(1),
            U256::MAX,
            U256::MAX,
            B256::repeat_byte(0xff),
            Bytes::from(vec![0xff; 32]),
        )
            .abi_encode_params();
        let edge_da_refs_input = edge_ref_message.repeat(SYSCOIN_DA_MAX_REFS_PER_BATCH);
        let commit_data = IExecutor::CommitBatchInfoZKsyncOS::from((
            u64::MAX,
            B256::repeat_byte(0xff),
            U256::MAX,
            U256::MAX,
            B256::repeat_byte(0xff),
            B256::repeat_byte(0xff),
            B256::repeat_byte(0xff),
            L2DACommitmentScheme::BLOBS_ZKSYNC_OS,
            B256::repeat_byte(0xff),
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            U256::MAX,
            Bytes::from(vec![0xff; SYSCOIN_DA_MAX_BLOBS_PER_BATCH * 32]),
            Bytes::from(edge_da_refs_input),
            B256::repeat_byte(0xff),
            U256::MAX,
        ))
        .abi_encode();
        let prev_commit_data = IExecutor::StoredBatchInfo::from((
            u64::MAX,
            B256::repeat_byte(0xff),
            u64::MAX,
            U256::MAX,
            B256::repeat_byte(0xff),
            B256::repeat_byte(0xff),
            B256::repeat_byte(0xff),
            U256::MAX,
            B256::repeat_byte(0xff),
        ))
        .abi_encode();
        assert_eq!(commit_data.len(), 8_864);
        assert_eq!(prev_commit_data.len(), 288);
        let message = Zks2faMessage::VerifyBatch(VerifyBatch {
            request_id: u64::MAX,
            batch_number: u64::MAX,
            first_block_number: u64::MAX,
            last_block_number: u64::MAX,
            pubdata_mode: u8::MAX,
            commit_data: commit_data.into(),
            prev_commit_data: prev_commit_data.into(),
            execution_protocol_version: u16::MAX,
        });
        let encoded = message.encoded();
        assert_eq!(encoded.len(), 9_203);
        assert!(encoded.len() < MAX_VERIFY_BATCH_FRAME_BYTES);
        assert_eq!(
            Zks2faMessage::decode_message(&mut encoded.as_ref()).unwrap(),
            message
        );
    }
}
