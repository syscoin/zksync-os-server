use num_enum::TryFromPrimitive;

use super::ProtocolSemanticVersion;

/// Identifier of the proving harness that must be used to generate and verify proofs for a given execution version.
/// Unlike `ExecutionVersion`, this may change in _each_ protocol version, e.g. in patches.
/// The main difference is that even if the state transition function remains the same,
/// there might be changes in the proving circuit which would not change the outcome of execution,
/// but would require different proving and verification keys.
#[derive(Debug, Clone, Copy, TryFromPrimitive, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ProvingVersion {
    V8 = 8,
}

impl TryFrom<ProtocolSemanticVersion> for ProvingVersion {
    type Error = ProvingVersionError;

    fn try_from(version: ProtocolSemanticVersion) -> Result<Self, Self::Error> {
        match (version.major, version.minor, version.patch) {
            // SYSCOIN: Use one canonical proving lane, matching upstream protocol V32:
            // patched final zksync-os v0.4.0 with Airbender V8 / 100-bit parameters.
            (0, 32, 0) => Ok(ProvingVersion::V8),
            _ => Err(ProvingVersionError::UnsupportedVersion(version)),
        }
    }
}

impl ProvingVersion {
    /// SYSCOIN: Fail-closed sentinel until external security-100 keygen binds the canonical
    /// Syscoin app. The stock upstream V8 hash is deliberately not accepted: it binds a
    /// different program. Replace this value atomically with the generated Era verifier
    /// artifacts before enabling real proving.
    const V8_VK_HASH: &'static str =
        "0x0000000000000000000000000000000000000000000000000000000000000000";
    const V8_VK_REGENERATION_REQUIRED: bool = true;

    pub const fn requires_vk_regeneration(&self) -> bool {
        match self {
            Self::V8 => Self::V8_VK_REGENERATION_REQUIRED,
        }
    }

    /// Get the verification key hash associated with this execution version.
    pub fn vk_hash(&self) -> &'static str {
        match self {
            Self::V8 => Self::V8_VK_HASH,
        }
    }

    /// Try to get ExecutionVersion from verification key hash.
    pub fn try_from_vk_hash(vk_hash: &str) -> Result<Self, ProvingVersionError> {
        match vk_hash {
            Self::V8_VK_HASH => Ok(Self::V8),
            val => Err(ProvingVersionError::UnsupportedVkHash(val.to_string())),
        }
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ProvingVersionError {
    #[error("Protocol version does not correspond to a known proving version: {0}")]
    UnsupportedVersion(ProtocolSemanticVersion),
    #[error("Verification key hash does not correspond to a known proving version: {0}")]
    UnsupportedVkHash(String),
}

#[cfg(test)]
mod tests {
    use super::{ProvingVersion, ProvingVersionError};
    use crate::ProtocolSemanticVersion;

    #[test]
    fn version_mapping() {
        let test_vector = [((0, 32, 0), ProvingVersion::V8)];

        for ((major, minor, patch), expected) in test_vector.iter() {
            let version = ProtocolSemanticVersion::new(*major, *minor, *patch);
            let proving_version = ProvingVersion::try_from(version.clone())
                .unwrap_or_else(|e| panic!("Failed to convert version {version:?}: {e}"));
            assert_eq!(&proving_version, expected);
        }

        let unknown_versions = [
            (0, 29, 1),
            (0, 30, 0),
            (0, 30, 1),
            (0, 30, 2),
            (0, 30, 3),
            (0, 31, 1),
            (0, 31, 2),
            (0, 32, 1),
            (0, 33, 0),
            (1, 30, 1),
            (1, 31, 0),
        ];

        for (major, minor, patch) in unknown_versions.iter() {
            let version = ProtocolSemanticVersion::new(*major, *minor, *patch);
            let proving_version = ProvingVersion::try_from(version);
            assert!(matches!(
                proving_version,
                Err(ProvingVersionError::UnsupportedVersion(_))
            ));
        }
    }

    #[test]
    fn vk_hash_mapping() {
        let test_vector = [(ProvingVersion::V8, ProvingVersion::V8_VK_HASH)];

        for (proving_version, expected_vk_hash) in test_vector.iter() {
            let vk_hash = proving_version.vk_hash();
            assert_eq!(vk_hash, *expected_vk_hash);

            let parsed_proving_version =
                ProvingVersion::try_from_vk_hash(vk_hash).unwrap_or_else(|e| {
                    panic!("Failed to convert vk_hash {vk_hash} back to proving version: {e}")
                });
            assert_eq!(&parsed_proving_version, proving_version);
        }

        let unknown_hash = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let proving_version = ProvingVersion::try_from_vk_hash(unknown_hash);
        assert!(matches!(
            proving_version,
            Err(ProvingVersionError::UnsupportedVkHash(_))
        ));
    }

    #[test]
    fn canonical_v8_vk_is_explicitly_blocked_until_keygen() {
        assert!(ProvingVersion::V8.requires_vk_regeneration());
    }
}
