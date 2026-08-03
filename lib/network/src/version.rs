//! Support for representing the version of the `zks` protocol.
//!
//! Version history:
//! - `zks/0` — test-only, never registered in production.
//! - `zks/1`–`zks/4` — retired and removed. `zks/1`/`zks/2` were replay-only on older record
//!   encodings; `zks/3`/`zks/4` additionally carried the verifier messages inline (message IDs
//!   0x02–0x06) before those moved to the standalone `zks_2fa` subprotocol.
//! - `zks/5` — current version; replay-only, `v3` record encoding.
//!
//! Versions evolve additively and are removed only in a later breaking release once the whole
//! fleet speaks a newer one; retired version numbers are never reused. See the "Version
//! lifecycle" section in `docs/src/design/devp2p.md` before touching this file.

use crate::wire::message::ZksMessageId;
use crate::wire::replays::{WireReplayRecord, v0, v1, v2, v4};
use alloy::primitives::bytes::BufMut;
use alloy::rlp::{Decodable, Encodable, Error as RlpError};
use std::fmt::Debug;

/// Type-level specification for a `zks` protocol version and its pinned wire formats.
pub trait ZksProtocolVersionSpec: Debug + Send + Sync + Unpin + Clone + 'static {
    /// Wire format for replay record.
    type Record: WireReplayRecord;

    /// Version number matching this protocol version.
    const VERSION: ZksVersion;
}

/// Test-only protocol version whose replay record preserves just the block number.
///
/// Keeping this deliberately lossy version makes capability-negotiation tests able to observe
/// which version the peers selected.
#[derive(Debug, Clone)]
pub struct ZksProtocolV0;

impl ZksProtocolVersionSpec for ZksProtocolV0 {
    type Record = v0::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks0;
}

/// Protocol version 5 supports replay streaming only via `GetBlockReplays` and `BlockReplays`.
/// The verifier handshake and batch verification live in the standalone `zks_2fa` subprotocol.
#[derive(Debug, Clone)]
pub struct ZksProtocolV5;

impl ZksProtocolVersionSpec for ZksProtocolV1 {
    type Record = v1::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks1;
}

/// Protocol version 2 keeps the replay transport from v1 but upgrades the replay record encoding.
#[derive(Debug, Clone)]
pub struct ZksProtocolV2;

impl ZksProtocolVersionSpec for ZksProtocolV2 {
    type Record = v2::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks2;
}

/// Protocol version 3 keeps replay record encoding from v2 and adds verifier-related messages:
/// `VerifierRoleRequest`, `VerifierChallenge`, `VerifierAuth`, `VerifyBatch`, and
/// `VerifyBatchResult`.
#[derive(Debug, Clone)]
pub struct ZksProtocolV3;

impl ZksProtocolVersionSpec for ZksProtocolV3 {
    type Record = v2::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks3;
}

/// Protocol version 4 keeps the replay transport from v3 but carries canonical upgrade hashes in
/// replay records.
///
/// SYSCOIN: This is the v31 launch wire shape. No Syscoin production network has used the
/// previous zks/4 replay layout, so redefine zks/4 here instead of adding a needless zks/5.
#[derive(Debug, Clone)]
pub struct ZksProtocolV4;

impl ZksProtocolVersionSpec for ZksProtocolV4 {
    type Record = v4::ReplayRecord;

    const VERSION: ZksVersion = ZksVersion::Zks5;
}

/// Error returned when a byte does not identify a registered [`ZksVersion`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Unknown zks protocol version: {0}")]
pub struct ParseVersionError(String);

/// The `zks` protocol version.
///
/// Discriminants are the version numbers advertised in the RLPx capability list. Versions 1-4 are
/// retired and their numbers must not be reused.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum ZksVersion {
    /// The `zks` protocol version 0. Only used for testing.
    Zks0 = 0,
    /// The `zks` protocol version 5. Replay-only; verifier messages moved to `zks_2fa`.
    Zks5 = 5,
}

impl ZksVersion {
    /// The latest registered `zks` version.
    pub const LATEST: Self = Self::Zks5;
}

/// Converts a `u8` into a registered [`ZksVersion`].
///
/// # Example
/// ```
/// use zksync_os_network::version::ZksVersion;
///
/// let version = ZksVersion::try_from(5).unwrap();
/// assert_eq!(version, ZksVersion::Zks5);
/// ```
impl TryFrom<u8> for ZksVersion {
    type Error = ParseVersionError;

    #[inline]
    fn try_from(u: u8) -> Result<Self, Self::Error> {
        match u {
            0 => Ok(Self::Zks0),
            1 => Ok(Self::Zks1),
            2 => Ok(Self::Zks2),
            3 => Ok(Self::Zks3),
            4 => Ok(Self::Zks4),
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

#[cfg(test)]
mod tests {
    use super::ZksVersion;

    #[test]
    fn test_zks_version_rlp_encode() {
        // Version 0 is purposefully left out as it encodes to 0x80 (prefix for 0-length string)
        let versions = [
            ZksVersion::Zks1,
            ZksVersion::Zks2,
            ZksVersion::Zks3,
            ZksVersion::Zks4,
        ];

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
            (3_u8, Ok(ZksVersion::Zks3)),
            (4_u8, Ok(ZksVersion::Zks4)),
            (5_u8, Err(RlpError::Custom("invalid zks version"))),
        ];

        for (input, expected) in test_cases {
            let mut encoded = BytesMut::new();
            input.encode(&mut encoded);

            let mut slice = encoded.as_ref();
            let result = ZksVersion::decode(&mut slice);
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn test_message_counts_match_protocol_surface() {
        let test_cases = [
            (ZksVersion::Zks0, 2),
            (ZksVersion::Zks1, 2),
            (ZksVersion::Zks2, 2),
            (ZksVersion::Zks3, 7),
            (ZksVersion::Zks4, 7),
        ];

        for (version, expected_count) in test_cases {
            assert_eq!(version.message_count(), expected_count);
        }
    }

    #[test]
    fn test_supports_message_matches_version_capabilities() {
        let old_messages = [ZksMessageId::GetBlockReplays, ZksMessageId::BlockReplays];
        let new_messages = [
            ZksMessageId::VerifierRoleRequest,
            ZksMessageId::VerifierChallenge,
            ZksMessageId::VerifierAuth,
            ZksMessageId::VerifyBatch,
            ZksMessageId::VerifyBatchResult,
        ];

        for version in [ZksVersion::Zks0, ZksVersion::Zks1, ZksVersion::Zks2] {
            for message in old_messages {
                assert!(version.supports_message(message));
            }
            for message in new_messages {
                assert!(!version.supports_message(message));
            }
        }

        for version in [ZksVersion::Zks3, ZksVersion::Zks4] {
            for message in old_messages.into_iter().chain(new_messages) {
                assert!(version.supports_message(message));
            }
        }
        assert!(ZksVersion::try_from(6).is_err());
    }
}

