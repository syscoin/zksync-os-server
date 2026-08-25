// SYSCOIN: The V2 auth digest binds a BE32 chain ID and both authenticated RLPx PeerIds.
use alloy::primitives::{Address, B256, Bytes, Signature, SignatureError, U256, keccak256};
use alloy_rlp::{RlpDecodable, RlpEncodable};
use reth_network_peers::PeerId;

/// Request to treat the current session as a verifier session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
pub struct VerifierRoleRequest {}

/// Main-node challenge used to authenticate a verifier session.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
pub struct VerifierChallenge {
    pub nonce: B256,
}

/// External-node authentication response proving control of the verifier signing key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
pub struct VerifierAuth {
    pub signature: Bytes,
}

/// SYSCOIN: V2 domain separator for verifier-session authentication signatures. V2 binds the
/// execution chain and both ordered RLPx identities so responses cannot cross networks or pairs.
const VERIFIER_AUTH_DOMAIN: &[u8] = b"zksync-os:verifier-auth:v2";

/// SYSCOIN: Returns the V2 prehash over `(chain ID as BE32, main-node PeerId, verifier PeerId,
/// nonce)` in that exact order. The wire challenge remains a nonce; the chain and both authenticated
/// RLPx identities are local facts.
pub(crate) fn verifier_auth_prehash(
    chain_id: u64,
    main_node_peer_id: PeerId,
    verifier_peer_id: PeerId,
    nonce: B256,
) -> B256 {
    let chain_id = U256::from(chain_id).to_be_bytes::<32>();
    keccak256(
        [
            VERIFIER_AUTH_DOMAIN,
            &chain_id,
            main_node_peer_id.as_slice(),
            verifier_peer_id.as_slice(),
            nonce.as_slice(),
        ]
        .concat(),
    )
}

/// SYSCOIN: Recovers only a canonical low-s, Electrum-encoded verifier signature. The auth
/// transcript is unreleased, so accepting alternate encodings would needlessly widen its wire form.
pub(crate) fn recover_verifier_signer(
    chain_id: u64,
    main_node_peer_id: PeerId,
    verifier_peer_id: PeerId,
    nonce: B256,
    signature: &[u8],
) -> Result<Address, SignatureError> {
    let signature = parse_canonical_recoverable_signature(
        signature,
        "noncanonical high-s verifier auth signature",
    )?;
    signature.recover_address_from_prehash(&verifier_auth_prehash(
        chain_id,
        main_node_peer_id,
        verifier_peer_id,
        nonce,
    ))
}

/// SYSCOIN: Validates the unique Electrum encoding used by verifier batch approvals before the
/// network consumes their exact request reservation. Cryptographic recovery remains collector-owned.
pub(crate) fn validate_canonical_batch_signature(signature: &[u8]) -> Result<(), SignatureError> {
    parse_canonical_recoverable_signature(signature, "noncanonical high-s batch approval signature")
        .map(|_| ())
}

// SYSCOIN: Authentication and batch approvals share one strict transport encoding: exactly 65
// bytes, Electrum parity 27/28, valid secp256k1 scalars, and the unique low-s representative.
fn parse_canonical_recoverable_signature(
    signature: &[u8],
    high_s_error: &'static str,
) -> Result<Signature, SignatureError> {
    let signature: &[u8; 65] = signature
        .try_into()
        .map_err(|_| SignatureError::FromBytes("expected exactly 65 bytes"))?;
    if !matches!(signature[64], 27 | 28) {
        return Err(SignatureError::InvalidParity(u64::from(signature[64])));
    }
    let signature = Signature::from_raw_array(signature)?;
    if signature.normalize_s().is_some() {
        return Err(SignatureError::FromBytes(high_s_error));
    }
    Ok(signature)
}

#[cfg(test)]
mod tests {
    use super::{SignatureError, recover_verifier_signer, verifier_auth_prehash};
    use alloy::primitives::{B256, B512, Signature, U256, uint};
    use alloy::signers::SignerSync;
    use alloy::signers::local::PrivateKeySigner;
    use std::str::FromStr;

    const TEST_CHAIN_ID: u64 = 57_057;
    const SECP256K1_ORDER: U256 =
        uint!(0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141_U256);

    #[test]
    fn verifier_auth_round_trip_recovers_signer() {
        let signer = PrivateKeySigner::from_str(
            "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110",
        )
        .unwrap();
        let main_node_peer_id = B512::repeat_byte(0x11);
        let verifier_peer_id = B512::repeat_byte(0x22);
        let nonce = B256::repeat_byte(0xAB);
        let signature = signer
            .sign_hash_sync(&verifier_auth_prehash(
                TEST_CHAIN_ID,
                main_node_peer_id,
                verifier_peer_id,
                nonce,
            ))
            .unwrap();

        let recovered = recover_verifier_signer(
            TEST_CHAIN_ID,
            main_node_peer_id,
            verifier_peer_id,
            nonce,
            &signature.as_bytes(),
        )
        .unwrap();
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn verifier_auth_signature_cannot_be_relayed_to_another_peer_pair() {
        let signer = PrivateKeySigner::from_str(
            "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110",
        )
        .unwrap();
        let main_node_peer_id = B512::repeat_byte(0x11);
        let verifier_peer_id = B512::repeat_byte(0x22);
        let nonce = B256::repeat_byte(0xAB);
        let signature = signer
            .sign_hash_sync(&verifier_auth_prehash(
                TEST_CHAIN_ID,
                main_node_peer_id,
                verifier_peer_id,
                nonce,
            ))
            .unwrap();

        for (other_main_node, other_verifier) in [
            (B512::repeat_byte(0x33), verifier_peer_id),
            (main_node_peer_id, B512::repeat_byte(0x44)),
            (verifier_peer_id, main_node_peer_id),
        ] {
            let relayed = recover_verifier_signer(
                TEST_CHAIN_ID,
                other_main_node,
                other_verifier,
                nonce,
                &signature.as_bytes(),
            )
            .unwrap();
            assert_ne!(relayed, signer.address());
        }
    }

    #[test]
    fn verifier_auth_signature_cannot_be_relayed_to_another_chain() {
        let signer = PrivateKeySigner::from_str(
            "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110",
        )
        .unwrap();
        let main_node_peer_id = B512::repeat_byte(0x11);
        let verifier_peer_id = B512::repeat_byte(0x22);
        let nonce = B256::repeat_byte(0xAB);
        let signature = signer
            .sign_hash_sync(&verifier_auth_prehash(
                TEST_CHAIN_ID,
                main_node_peer_id,
                verifier_peer_id,
                nonce,
            ))
            .unwrap();

        let relayed = recover_verifier_signer(
            TEST_CHAIN_ID + 1,
            main_node_peer_id,
            verifier_peer_id,
            nonce,
            &signature.as_bytes(),
        )
        .unwrap();
        assert_ne!(relayed, signer.address());
    }

    #[test]
    fn malformed_signature_is_rejected() {
        let err = recover_verifier_signer(
            TEST_CHAIN_ID,
            B512::repeat_byte(0x11),
            B512::repeat_byte(0x22),
            B256::repeat_byte(0xAB),
            &[7u8; 64],
        )
        .unwrap_err();
        assert!(matches!(
            err,
            alloy::primitives::SignatureError::FromBytes(_)
        ));
    }

    // SYSCOIN: Authentication uses one canonical low-s encoding even though the malleated form
    // recovers the same accepted verifier address.
    #[test]
    fn verifier_auth_rejects_malleated_high_s_signature() {
        let signer = PrivateKeySigner::from_str(
            "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110",
        )
        .unwrap();
        let main_node_peer_id = B512::repeat_byte(0x11);
        let verifier_peer_id = B512::repeat_byte(0x22);
        let nonce = B256::repeat_byte(0xAB);
        let digest =
            verifier_auth_prehash(TEST_CHAIN_ID, main_node_peer_id, verifier_peer_id, nonce);
        let canonical = signer.sign_hash_sync(&digest).unwrap();
        let mut malleated = canonical.as_bytes();
        let high_s = SECP256K1_ORDER - canonical.s();
        malleated[32..64].copy_from_slice(&high_s.to_be_bytes::<32>());
        malleated[64] = if malleated[64] == 27 { 28 } else { 27 };

        let alternate = Signature::from_raw_array(&malleated).unwrap();
        assert_eq!(
            alternate.recover_address_from_prehash(&digest).unwrap(),
            signer.address()
        );
        assert!(matches!(
            recover_verifier_signer(
                TEST_CHAIN_ID,
                main_node_peer_id,
                verifier_peer_id,
                nonce,
                &malleated,
            ),
            Err(SignatureError::FromBytes(
                "noncanonical high-s verifier auth signature"
            ))
        ));
    }

    // SYSCOIN: Alloy accepts normalized and EIP-155-style parity bytes, but `zks_2fa` auth is
    // uniquely encoded with Electrum 27/28 parity.
    #[test]
    fn verifier_auth_rejects_alternate_parity_encodings() {
        let signer = PrivateKeySigner::from_str(
            "0x7726827caac94a7f9e1b160f7ea819f172f7b6f9d2a97f992c38edeab82d4110",
        )
        .unwrap();
        let main_node_peer_id = B512::repeat_byte(0x11);
        let verifier_peer_id = B512::repeat_byte(0x22);
        let nonce = B256::repeat_byte(0xAB);
        let mut signature = signer
            .sign_hash_sync(&verifier_auth_prehash(
                TEST_CHAIN_ID,
                main_node_peer_id,
                verifier_peer_id,
                nonce,
            ))
            .unwrap()
            .as_bytes();

        for parity in [signature[64] - 27, 35 + (signature[64] - 27)] {
            signature[64] = parity;
            assert!(Signature::from_raw_array(&signature).is_ok());
            assert!(matches!(
                recover_verifier_signer(
                    TEST_CHAIN_ID,
                    main_node_peer_id,
                    verifier_peer_id,
                    nonce,
                    &signature,
                ),
                Err(SignatureError::InvalidParity(actual)) if actual == u64::from(parity)
            ));
        }
    }
}
