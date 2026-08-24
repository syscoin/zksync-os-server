use alloy::primitives::{Address, B256, Signature as AlloySignature, SignatureError, U256};
use alloy::signers::Signer;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::{Eip712Domain, SolStruct};
use serde::{Deserialize, Serialize};
use zksync_os_contract_interface::calldata::encode_commit_batch_data;
use zksync_os_contract_interface::models::{CommitBatchInfo, StoredBatchInfo};
use zksync_os_types::ProtocolSemanticVersion;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchSignatureSet(Vec<ValidatedBatchSignature>);

#[derive(Debug, thiserror::Error)]
pub enum BatchSignatureSetError {
    #[error("Duplicated signature")]
    DuplicatedSignature,
}

impl BatchSignatureSet {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        BatchSignatureSet(Vec::new())
    }

    pub fn push(
        &mut self,
        signature: ValidatedBatchSignature,
    ) -> Result<(), BatchSignatureSetError> {
        if self.0.contains(&signature) {
            return Err(BatchSignatureSetError::DuplicatedSignature);
        }
        self.0.push(signature);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn to_vec(&self) -> &Vec<ValidatedBatchSignature> {
        &self.0
    }

    /// Remove signatures not found on allowed list
    pub fn filter(mut self, allowed_signers: &[Address]) -> Self {
        self.0.retain(|s| allowed_signers.contains(&s.signer));
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BatchSignature(AlloySignature);

impl BatchSignature {
    /// Sign a batch for `commitBatchesMultisig`
    pub async fn sign_batch(
        prev_batch_info: &StoredBatchInfo,
        commit_batch_info: &CommitBatchInfo,
        diamond_proxy_sl: Address,
        sl_chain_id: u64,
        multisig_committer: Address,
        protocol_version: &ProtocolSemanticVersion,
        private_key: &PrivateKeySigner,
    ) -> Self {
        let digest = eip712_multisig_digest(
            prev_batch_info,
            commit_batch_info,
            diamond_proxy_sl,
            sl_chain_id,
            multisig_committer,
            protocol_version,
        );
        let signature = private_key.sign_hash(&digest).await.unwrap();
        BatchSignature(signature)
    }

    pub fn verify_signature(
        self,
        prev_batch_info: &StoredBatchInfo,
        commit_batch_info: &CommitBatchInfo,
        diamond_proxy_sl: Address,
        sl_chain_id: u64,
        multisig_committer: Address,
        protocol_version: &ProtocolSemanticVersion,
    ) -> Result<ValidatedBatchSignature, SignatureError> {
        Ok(ValidatedBatchSignature {
            signer: self
                .0
                .recover_address_from_prehash(&eip712_multisig_digest(
                    prev_batch_info,
                    commit_batch_info,
                    diamond_proxy_sl,
                    sl_chain_id,
                    multisig_committer,
                    protocol_version,
                ))?,
            signature: self,
        })
    }
    pub fn into_raw(self) -> [u8; 65] {
        self.0.as_bytes()
    }

    pub fn from_raw_array(array: &[u8; 65]) -> Result<Self, SignatureError> {
        // SYSCOIN: Multisig signatures cross a trust boundary before signer recovery and set
        // deduplication. Accept only canonical Electrum parity and low-s form so the equivalent
        // `(r, n-s, !parity)` encoding cannot bypass one-signature-per-validator accounting.
        if !matches!(array[64], 27 | 28) {
            return Err(SignatureError::InvalidParity(u64::from(array[64])));
        }
        let signature = AlloySignature::from_raw_array(array)?;
        if signature.normalize_s().is_some() {
            return Err(SignatureError::FromBytes(
                "noncanonical high-s batch signature",
            ));
        }
        Ok(BatchSignature(signature))
    }
}

sol! {
    #[derive(Debug)]
    struct CommitBatchesMultisig {
        address chainAddress;
        uint256 processBatchFrom;
        uint256 processBatchTo;
        bytes batchData;
    }
}

/// Compute the full EIP-712 digest used by the `MultisigCommitter` contract
/// for the `commitBatchesMultisig` typed data, based on the given batch info
/// and L1 domain parameters.
fn eip712_multisig_digest(
    prev_batch_info: &StoredBatchInfo,
    commit_batch_info: &CommitBatchInfo,
    diamond_proxy_sl: Address,
    sl_chain_id: u64,
    multisig_committer: Address,
    protocol_version: &ProtocolSemanticVersion,
) -> B256 {
    let batch_data = encode_commit_batch_data(
        prev_batch_info,
        commit_batch_info.clone(),
        protocol_version.minor,
    );

    let message = CommitBatchesMultisig {
        chainAddress: diamond_proxy_sl,
        processBatchFrom: U256::from(commit_batch_info.batch_number),
        processBatchTo: U256::from(commit_batch_info.batch_number),
        batchData: batch_data.into(),
    };

    let domain = Eip712Domain {
        name: Some("MultisigCommitter".into()),
        version: Some("1".into()),
        chain_id: Some(U256::from(sl_chain_id)),
        verifying_contract: Some(multisig_committer),
        salt: None,
    };

    message.eip712_signing_hash(&domain)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatedBatchSignature {
    signature: BatchSignature,
    signer: Address,
}

impl ValidatedBatchSignature {
    pub fn signature(&self) -> &BatchSignature {
        &self.signature
    }

    pub fn signer(&self) -> &Address {
        &self.signer
    }
}

impl PartialEq for ValidatedBatchSignature {
    fn eq(&self, other: &Self) -> bool {
        self.signer == other.signer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::uint;
    use alloy::signers::SignerSync;
    use std::str::FromStr;

    const TEST_SIGNING_KEY: &str =
        "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110";
    const SECP256K1_ORDER: U256 =
        uint!(0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141_U256);

    // SYSCOIN: Reject the alternate high-s encoding before it can become a BatchSignature and
    // reach signer-based BatchSignatureSet deduplication.
    #[test]
    fn rejects_malleated_high_s_signature_before_signature_set() {
        let signer = PrivateKeySigner::from_str(TEST_SIGNING_KEY).unwrap();
        let digest = B256::repeat_byte(0x42);
        let canonical = signer.sign_hash_sync(&digest).unwrap();
        assert!(canonical.normalize_s().is_none());
        let canonical_raw = canonical.as_bytes();
        assert!(BatchSignature::from_raw_array(&canonical_raw).is_ok());

        let mut malleated = canonical_raw;
        let high_s = SECP256K1_ORDER - canonical.s();
        malleated[32..64].copy_from_slice(&high_s.to_be_bytes::<32>());
        malleated[64] = if canonical_raw[64] == 27 { 28 } else { 27 };

        let alternate = AlloySignature::from_raw_array(&malleated).unwrap();
        assert_eq!(
            alternate.recover_address_from_prehash(&digest).unwrap(),
            signer.address(),
            "the malleated form must demonstrate the same recovered validator",
        );
        assert!(alternate.normalize_s().is_some());
        let rejected_before_set = BatchSignature::from_raw_array(&malleated);
        assert!(matches!(
            rejected_before_set,
            Err(SignatureError::FromBytes(
                "noncanonical high-s batch signature"
            ))
        ));
    }

    // SYSCOIN: Alloy accepts normalized 0/1 parity, but the contract-facing multisig wire format
    // is uniquely encoded with Electrum 27/28 parity.
    #[test]
    fn rejects_normalized_parity_bytes() {
        let signer = PrivateKeySigner::from_str(TEST_SIGNING_KEY).unwrap();
        let mut raw = signer
            .sign_hash_sync(&B256::repeat_byte(0x24))
            .unwrap()
            .as_bytes();
        raw[64] -= 27;

        assert!(AlloySignature::from_raw_array(&raw).is_ok());
        assert!(matches!(
            BatchSignature::from_raw_array(&raw),
            Err(SignatureError::InvalidParity(parity)) if parity <= 1
        ));
    }
}
