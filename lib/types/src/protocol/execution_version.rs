use num_enum::TryFromPrimitive;

use super::ProtocolSemanticVersion;

/// Identifier of the MultiVM execution version that corresponds to a concrete state transition function.
/// Generally this is depicted by the minor of the protocol version, e.g. it can (but not guaranteed to) only change
/// if the minor of the protocol version changes.
#[derive(Debug, Clone, Copy, TryFromPrimitive, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ExecutionVersion {
    /// SYSCOIN: Canonical execution lane: patched final zksync-os v0.4.0.
    V7 = 7,
}

impl TryFrom<&ProtocolSemanticVersion> for ExecutionVersion {
    type Error = ExecutionVersionError;

    fn try_from(version: &ProtocolSemanticVersion) -> Result<Self, Self::Error> {
        // SYSCOIN: Fresh-only registry; do not silently execute historical protocol identities.
        match (version.major, version.minor, version.patch) {
            (0, 32, 0) => Ok(ExecutionVersion::V7),
            _ => Err(ExecutionVersionError::UnsupportedVersion(version.clone())),
        }
    }
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ExecutionVersionError {
    #[error("Protocol version does not correspond to a known execution version: {0}")]
    UnsupportedVersion(ProtocolSemanticVersion),
}

#[cfg(test)]
mod tests {
    use super::{ExecutionVersion, ExecutionVersionError};
    use crate::ProtocolSemanticVersion;

    #[test]
    fn version_mapping() {
        // When adding new versions here, make sure to also update `unknown_versions` so that it makes sure
        // that the (new) next protocol version is unknown.
        let test_vector = [((0, 32, 0), ExecutionVersion::V7)];

        for ((major, minor, patch), expected) in test_vector.iter() {
            let version = ProtocolSemanticVersion::new(*major, *minor, *patch);
            let exec_version = ExecutionVersion::try_from(&version)
                .unwrap_or_else(|e| panic!("Failed to convert version {version:?}: {e}"));
            assert_eq!(&exec_version, expected);
        }

        let unknown_versions = [
            (0, 29, 1),
            (0, 30, 2),
            (0, 31, 0),
            (0, 31, 1),
            (0, 32, 1),
            (0, 34, 0),
            (1, 31, 0),
        ];

        for (major, minor, patch) in unknown_versions.iter() {
            let version = ProtocolSemanticVersion::new(*major, *minor, *patch);
            let exec_version = ExecutionVersion::try_from(&version);
            assert!(matches!(
                exec_version,
                Err(ExecutionVersionError::UnsupportedVersion(_))
            ));
        }
    }
}
