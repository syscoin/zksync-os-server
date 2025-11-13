use serde::{Deserialize, Serialize};

/// The chain pubdata mode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PubdataMode {
    Blobs,
    Calldata,
    Validium,
}

impl PubdataMode {
    pub fn da_commitment_scheme(&self) -> zksync_os_contract_interface::models::DACommitmentScheme {
        match self {
            Self::Blobs => zksync_os_contract_interface::models::DACommitmentScheme::BlobsZKsyncOS,
            Self::Calldata => {
                zksync_os_contract_interface::models::DACommitmentScheme::BlobsAndPubdataKeccak256
            }
            Self::Validium => zksync_os_contract_interface::models::DACommitmentScheme::EmptyNoDA,
        }
    }
}
