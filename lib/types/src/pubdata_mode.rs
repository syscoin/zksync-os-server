use serde::{Deserialize, Serialize};

/// The chain pubdata mode.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PubdataMode {
    Blobs = 0,
    /// SYSCOIN: Edge-chain Bitcoin DA represented by compact references on Gateway.
    RelayedL2Calldata = 3,
}

impl PubdataMode {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(PubdataMode::Blobs),
            3 => Some(PubdataMode::RelayedL2Calldata),
            _ => None,
        }
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn da_commitment_scheme(&self) -> zksync_os_contract_interface::models::DACommitmentScheme {
        match self {
            Self::Blobs => zksync_os_contract_interface::models::DACommitmentScheme::BlobsZKsyncOS,
            // SYSCOIN: edge chains settling to Gateway publish pubdata directly to Bitcoin DA and
            // send compact blob-hash references, not full relayed calldata.
            Self::RelayedL2Calldata => {
                zksync_os_contract_interface::models::DACommitmentScheme::BlobsZKsyncOS
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PubdataMode;

    #[test]
    fn only_canonical_syscoin_pubdata_modes_decode_from_wire() {
        assert_eq!(PubdataMode::from_u8(0), Some(PubdataMode::Blobs));
        assert_eq!(
            PubdataMode::from_u8(3),
            Some(PubdataMode::RelayedL2Calldata)
        );
        assert_eq!(PubdataMode::from_u8(1), None);
        assert_eq!(PubdataMode::from_u8(2), None);
    }

    #[test]
    fn legacy_pubdata_mode_names_do_not_deserialize() {
        assert!(serde_json::from_str::<PubdataMode>(r#""Calldata""#).is_err());
        assert!(serde_json::from_str::<PubdataMode>(r#""Validium""#).is_err());
    }
}
