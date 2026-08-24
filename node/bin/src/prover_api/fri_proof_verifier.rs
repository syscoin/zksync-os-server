use crate::prover_api::fri_job_manager::SubmitError;
use alloy::primitives::{B256, keccak256};
use riscv_transpiler::common_constants::{
    BLAKE2S_DELEGATION_CSR_REGISTER, REDUCED_MACHINE_CIRCUIT_FAMILY_IDX,
};
use verifier_common::SecurityModel;
use zksync_os_batch_types::batcher_model::BatchMetadata;
use zksync_os_types::ProvingVersion;

// SYSCOIN: Verify only the canonical V8 unrolled proof format and app commitment.
pub fn verify_real_fri_proof_bytes(
    batch_metadata: &BatchMetadata,
    proof_bytes: &[u8],
) -> Result<(), SubmitError> {
    let proving_version = batch_metadata.proving_version().map_err(|err| {
        SubmitError::TemporaryInternal(format!("cannot determine proving version: {err:#}"))
    })?;
    debug_assert_eq!(proving_version, ProvingVersion::V8);
    let expected_hash_u32s = expected_public_input_registers(batch_metadata)?;
    let batch_number = batch_metadata.batch_info.commit_info.batch_number;
    let program_proof = decode_canonical_real_fri_proof(proof_bytes)?;
    verify_fri_proof(expected_hash_u32s, &program_proof, batch_number)
}

/// SYSCOIN: Bincode decoders accept a valid prefix by design. Prover proofs are capabilities and
/// durable artifacts, so require full consumption to prevent authenticated storage/response
/// amplification with an otherwise-valid proof plus arbitrary trailing bytes.
pub fn decode_canonical_real_fri_proof(
    proof_bytes: &[u8],
) -> Result<execution_utils::unrolled::UnrolledProgramProof, SubmitError> {
    decode_canonical_bincode(proof_bytes)
}

// SYSCOIN: Keep canonical-consumption enforcement independently regression-testable without a
// multi-megabyte cryptographic fixture; the production wrapper above fixes `T` to the V8 proof.
fn decode_canonical_bincode<T: serde::de::DeserializeOwned>(
    proof_bytes: &[u8],
) -> Result<T, SubmitError> {
    let (proof, consumed) =
        bincode::serde::decode_from_slice(proof_bytes, bincode::config::standard())
            .map_err(SubmitError::DeserializationFailed)?;
    if consumed != proof_bytes.len() {
        return Err(SubmitError::InvalidProofShape(format!(
            "canonical proof consumed {consumed} of {} bytes",
            proof_bytes.len()
        )));
    }
    Ok(proof)
}

/// Expected batch public-input hash, as the final register values a valid FRI proof of this
/// batch must expose.
///
/// The final-v0.4 batch public input is
/// `keccak(state_before || state_after || chain_config_hash || batch_output)`, where
/// `batch_output` uses the 0.4.0 layout without the leading chain id
/// (see [`PendingBatchInfo::batch_output_hash`](zksync_os_batch_types::PendingBatchInfo)).
pub fn expected_public_input_registers(
    batch_metadata: &BatchMetadata,
) -> Result<[u32; 8], SubmitError> {
    let state_before = batch_metadata.previous_stored_batch_info.state_commitment;
    let batch_info = &batch_metadata.batch_info;
    let chain_config_hash =
        zksync_os_native_pig::chain_config_hash(batch_info.commit_info.chain_id)
            // SYSCOIN: A local chain-config lookup failure is retryable and must retain the FRI lease.
            .map_err(|err| {
                SubmitError::TemporaryInternal(format!("cannot compute chain config hash: {err:#}"))
            })?;
    let hash = keccak256(
        [
            state_before.0,
            batch_info.commit_info.new_state_commitment.0,
            chain_config_hash.0,
            batch_info.batch_output_hash().0,
        ]
        .concat(),
    );
    Ok(hash_as_register_values(hash))
}

/// SYSCOIN: Verifies a FRI proof from the final-v0.4 Airbender unrolled stack.
///
/// V8 provers submit an `UnrolledProgramProof` recursed up to the *unified* layer. The unified
/// recursion program is app-independent and embedded in `execution_utils`, so verification
/// needs no app binary: we run the native unified-layer statement verifier to trustlessly extract
/// the final register values, check that the proof's recursion chain is rooted in the V8 batch
/// program (registers `[8..16]`), and compare registers `[..8]` against the expected batch
/// public input hash.
pub fn verify_fri_proof(
    expected_hash_u32s: [u32; 8],
    proof: &execution_utils::unrolled::UnrolledProgramProof,
    batch_number: u64,
) -> Result<(), SubmitError> {
    // SYSCOIN: Startup already blocks external proving while the VK is pending, but durable proof
    // recovery and library callers also reach this verifier directly. Keep the app binding itself
    // fail-closed until the rebuilt guest identity is installed.
    if v8_verifier::V8_APP_IDENTITY_REGENERATION_REQUIRED {
        return Err(SubmitError::TemporaryInternal(format!(
            "canonical V8 app identity regeneration is required for zksync-os tree {} (app md5 {})",
            v8_verifier::V8_APP_IDENTITY_SOURCE_TREE,
            v8_verifier::V8_APP_BIN_MD5,
        )));
    }

    validate_v8_proof_shape(proof)?;

    // Cheap consistency check of the carried chain fields (mirrors the airbender CLI's
    // `validate_recursion_chain`).
    v8_verifier::validate_recursion_chain(proof).map_err(|msg| {
        tracing::warn!(
            batch_number,
            msg,
            "V8 proof carries an invalid recursion chain"
        );
        // SYSCOIN: This is authenticated prover input, not a transient server fault; classify it
        // as a definitive shape rejection so the exact lease is revoked for immediate repick.
        SubmitError::InvalidProofShape(format!("invalid V8 proof recursion chain: {msg}"))
    })?;

    // The unified-layer verifier returns Err on invalid proofs, but its internals can
    // still assert (panic) on malformed input; catch it so a bad proof is reported -
    // and persisted for debugging - as a verification failure.
    let proof_final_register_values: [u32; 16] =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| v8_verifier::verify(proof)))
            .unwrap_or_else(|_| {
                tracing::warn!(
                    batch_number,
                    "V8 unified-layer verifier panicked on a malformed proof"
                );
                Err(())
            })
            .map_err(|()| SubmitError::FriProofVerificationError {
                expected_hash_u32s,
                // The verifier failed before producing register values.
                proof_final_register_values: [0u32; 16],
            })?;

    // Bind the proof to the V8 batch program. The unified-layer verifier is app-independent
    // and authenticates the recursion chain it actually proved in registers [8..16]; only a
    // chain rooted in the V8 batch program's end params is acceptable — otherwise any guest
    // program exposing the right public-input hash would pass.
    let expected_chain = v8_verifier::unified_level_data().expected_chain;
    if proof_final_register_values[8..16] != expected_chain {
        tracing::warn!(
            batch_number,
            ?expected_chain,
            actual_chain = ?&proof_final_register_values[8..16],
            "V8 proof proves a different program (recursion chain mismatch)"
        );
        return Err(SubmitError::FriProofVerificationError {
            expected_hash_u32s,
            proof_final_register_values,
        });
    }

    check_public_input(expected_hash_u32s, proof_final_register_values)
}

/// SYSCOIN: Reject shapes the native verifier may flatten but the SNARK wrapper cannot encode.
#[derive(Clone, Copy, Debug)]
struct V8ProofShape {
    circuit_family_entries: usize,
    reduced_family_proofs: Option<usize>,
    init_and_teardown_proofs: usize,
    delegation_entries: usize,
    blake2s_delegation_proofs: Option<usize>,
}

impl V8ProofShape {
    fn from_proof(proof: &execution_utils::unrolled::UnrolledProgramProof) -> Self {
        Self {
            circuit_family_entries: proof.circuit_families_proofs.len(),
            reduced_family_proofs: proof
                .circuit_families_proofs
                .get(&REDUCED_MACHINE_CIRCUIT_FAMILY_IDX)
                .map(Vec::len),
            init_and_teardown_proofs: proof.inits_and_teardowns_proofs.len(),
            delegation_entries: proof.delegation_proofs.len(),
            blake2s_delegation_proofs: proof
                .delegation_proofs
                .get(&BLAKE2S_DELEGATION_CSR_REGISTER)
                .map(Vec::len),
        }
    }

    fn validate(self) -> Result<(), String> {
        if self.circuit_family_entries != 1 {
            return Err(format!(
                "expected exactly one circuit-family entry, got {}",
                self.circuit_family_entries
            ));
        }

        let expected_unified_proofs =
            execution_utils::unified_recursion_target_family_proofs(SecurityModel::Security100);
        if self.reduced_family_proofs != Some(expected_unified_proofs) {
            return Err(format!(
                "expected circuit family {REDUCED_MACHINE_CIRCUIT_FAMILY_IDX} with {expected_unified_proofs} unified proofs, got {:?}",
                self.reduced_family_proofs
            ));
        }

        if self.init_and_teardown_proofs != 0 {
            return Err(format!(
                "expected no init/teardown proofs, got {}",
                self.init_and_teardown_proofs
            ));
        }

        if self.delegation_entries != 1 {
            return Err(format!(
                "expected exactly one delegation entry, got {}",
                self.delegation_entries
            ));
        }

        if self.blake2s_delegation_proofs != Some(1) {
            return Err(format!(
                "expected delegation {BLAKE2S_DELEGATION_CSR_REGISTER:#x} with one Blake2s proof, got {:?}",
                self.blake2s_delegation_proofs
            ));
        }

        Ok(())
    }
}

fn validate_v8_proof_shape(
    proof: &execution_utils::unrolled::UnrolledProgramProof,
) -> Result<(), SubmitError> {
    V8ProofShape::from_proof(proof)
        .validate()
        .map_err(SubmitError::InvalidProofShape)
}

/// Compares the expected public-input hash with the first 8 final register values.
fn check_public_input(
    expected_hash_u32s: [u32; 8],
    proof_final_register_values: [u32; 16],
) -> Result<(), SubmitError> {
    (proof_final_register_values[..8] == expected_hash_u32s)
        .then_some(())
        .ok_or(SubmitError::FriProofVerificationError {
            expected_hash_u32s,
            proof_final_register_values,
        })
}

/// V8 unified-layer verification internals: the pinned airbender verifier (see
/// `execution_utils` in the root `Cargo.toml`) plus the expected recursion chain that
/// binds proofs to the V8 batch program.
mod v8_verifier {
    use execution_utils::setups::{
        CompiledCircuitsSet, binary_u8_to_u32, get_unified_circuit_artifact_for_machine_type,
        pad_bytecode_bytes_for_proving, pad_bytecode_for_proving,
    };
    use execution_utils::unified_circuit::{
        compute_unified_setup_for_machine_configuration, verify_proof_in_unified_layer,
    };
    use execution_utils::unrolled::{
        UnrolledProgramProof, UnrolledProgramSetup, compute_setup_for_machine_configuration,
    };
    use execution_utils::verifier_binaries::recursion_artifact;
    use execution_utils::{RecursionArtifact, RecursionLayer};
    use riscv_transpiler::cycle::IWithoutByteAccessIsaConfigWithDelegation;
    use verifier_common::SecurityModel;
    use verifier_common::transcript::Blake2sBufferingTranscript;

    /// The canonical V8 lane proves at 100-bit; this selects the recursion verifier binaries the chain below is
    /// continued through, so it must match the prover's `PROVING_SECURITY_LEVEL`.
    const SECURITY: SecurityModel = SecurityModel::Security100;

    /// SYSCOIN: Exact reviewed guest source awaiting the reproducible app rebuild. These
    /// sentinels are replaced together from Airbender `end_params` output before keygen is
    /// authorized; no prior app identity is valid for this tree.
    pub(super) const V8_APP_IDENTITY_SOURCE_TREE: &str = "20dc217bbd535877f600df88bd7e2966d3d9b43a";
    pub(super) const V8_APP_BIN_MD5: &str = "00000000000000000000000000000000";
    pub(super) const V8_APP_IDENTITY_REGENERATION_REQUIRED: bool = true;

    /// `end_params` is derived from the app binary alone. The Security100 chain continues those
    /// params through the pinned unrolled and unified recursion artifacts.
    pub(super) const V8_APP_END_PARAMS: [u32; 8] = [0; 8];
    pub(super) const V8_SECURITY100_EXPECTED_CHAIN: [u32; 8] = [0; 8];

    pub(super) struct UnifiedLevelData {
        setup: UnrolledProgramSetup,
        layouts: CompiledCircuitsSet,
        /// Expected recursion chain hash (final registers `[8..16]`) of an honest V8 proof:
        /// `begin(app) -> continue(unrolled recursion) -> continue(unified recursion)`,
        /// mirroring the airbender CLI's `ensure_recursion_chain_binds_program` derivation.
        pub(super) expected_chain: [u32; 8],
    }

    fn padded_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut padded = bytes.to_vec();
        pad_bytecode_bytes_for_proving(&mut padded);
        padded
    }

    /// The unified-layer setup, circuit layouts and expected recursion chain are derived from
    /// the embedded recursion programs once and cached.
    pub(super) fn unified_level_data() -> &'static UnifiedLevelData {
        static DATA: std::sync::OnceLock<UnifiedLevelData> = std::sync::OnceLock::new();
        DATA.get_or_init(|| {
            let unified_bin =
                recursion_artifact(SECURITY, RecursionLayer::Unified, RecursionArtifact::Bin);
            let unified_text =
                recursion_artifact(SECURITY, RecursionLayer::Unified, RecursionArtifact::Txt);
            let setup = compute_unified_setup_for_machine_configuration::<
                IWithoutByteAccessIsaConfigWithDelegation,
            >(&padded_bytes(unified_bin), &padded_bytes(unified_text));
            let mut padded_bin_u32 = binary_u8_to_u32(unified_bin);
            pad_bytecode_for_proving(&mut padded_bin_u32);
            let layouts = get_unified_circuit_artifact_for_machine_type::<
                IWithoutByteAccessIsaConfigWithDelegation,
            >(&padded_bin_u32);

            let unrolled_bin =
                recursion_artifact(SECURITY, RecursionLayer::Unrolled, RecursionArtifact::Bin);
            let unrolled_text =
                recursion_artifact(SECURITY, RecursionLayer::Unrolled, RecursionArtifact::Txt);
            let unrolled_setup = compute_setup_for_machine_configuration::<
                IWithoutByteAccessIsaConfigWithDelegation,
            >(
                &padded_bytes(unrolled_bin), &padded_bytes(unrolled_text)
            );

            let (base_chain, base_preimage) =
                UnrolledProgramSetup::begin_recursion_chain(&V8_APP_END_PARAMS);
            let (unrolled_chain, unrolled_preimage) =
                UnrolledProgramSetup::continue_recursion_chain(
                    &unrolled_setup.end_params,
                    &base_chain,
                    &base_preimage,
                );
            let (expected_chain, _) = UnrolledProgramSetup::continue_recursion_chain(
                &setup.end_params,
                &unrolled_chain,
                &unrolled_preimage,
            );
            if !V8_APP_IDENTITY_REGENERATION_REQUIRED {
                assert_eq!(
                    expected_chain, V8_SECURITY100_EXPECTED_CHAIN,
                    "canonical V8 Security100 chain does not match the installed app identity"
                );
            }

            UnifiedLevelData {
                setup,
                layouts,
                expected_chain,
            }
        })
    }

    /// Checks that the chain fields carried by the proof are internally consistent
    /// (mirrors the airbender CLI's `validate_recursion_chain`).
    pub(super) fn validate_recursion_chain(proof: &UnrolledProgramProof) -> Result<(), String> {
        let Some(preimage) = proof.recursion_chain_preimage else {
            return Err("proof is missing recursion_chain_preimage".to_string());
        };
        let Some(hash) = proof.recursion_chain_hash else {
            return Err("proof is missing recursion_chain_hash".to_string());
        };
        let mut hasher = Blake2sBufferingTranscript::new();
        hasher.absorb(&preimage);
        if hasher.finalize().0 != hash {
            return Err("recursion chain hash mismatch".to_string());
        }
        Ok(())
    }

    /// Runs the unified-layer statement verifier and returns the authenticated final register
    /// values.
    pub(super) fn verify(proof: &UnrolledProgramProof) -> Result<[u32; 16], ()> {
        let data = unified_level_data();
        verify_proof_in_unified_layer(proof, &data.setup, &data.layouts, false, SECURITY)
    }
}

fn hash_as_register_values(hash: B256) -> [u32; 8] {
    hash.0
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().expect("Slice with incorrect length")))
        .collect::<Vec<u32>>()
        .try_into()
        .expect("Hash should be exactly 32 bytes long")
}

#[cfg(test)]
mod tests {
    use super::{V8ProofShape, decode_canonical_bincode, v8_verifier};
    use execution_utils::unified_recursion_target_family_proofs;
    use verifier_common::SecurityModel;

    fn canonical_security_100_shape() -> V8ProofShape {
        V8ProofShape {
            circuit_family_entries: 1,
            reduced_family_proofs: Some(unified_recursion_target_family_proofs(
                SecurityModel::Security100,
            )),
            init_and_teardown_proofs: 0,
            delegation_entries: 1,
            blake2s_delegation_proofs: Some(1),
        }
    }

    #[test]
    fn wrapper_compatible_security_100_shape_is_accepted() {
        canonical_security_100_shape().validate().unwrap();
    }

    // SYSCOIN: Bincode accepts a valid prefix; authenticated proof storage must not accept it.
    #[test]
    fn canonical_decoder_rejects_trailing_bytes() {
        let mut encoded = bincode::serde::encode_to_vec(42_u64, bincode::config::standard())
            .expect("u64 encoding succeeds");
        assert_eq!(decode_canonical_bincode::<u64>(&encoded).unwrap(), 42);
        encoded.push(0xaa);
        assert!(
            decode_canonical_bincode::<u64>(&encoded)
                .unwrap_err()
                .to_string()
                .contains("canonical proof consumed")
        );
    }

    #[test]
    fn extra_empty_delegation_entry_ignored_by_native_verifier_is_rejected() {
        let malicious_shape = V8ProofShape {
            // The canonical Blake2s entry is still valid; this models an additional unknown map
            // entry with an empty proof vector, which Airbender's native response flattener skips.
            delegation_entries: 2,
            ..canonical_security_100_shape()
        };

        let err = malicious_shape.validate().unwrap_err();
        assert_eq!(err, "expected exactly one delegation entry, got 2");
    }

    #[test]
    fn other_wrapper_incompatible_shapes_are_rejected() {
        let extra_empty_family = V8ProofShape {
            circuit_family_entries: 2,
            ..canonical_security_100_shape()
        };
        assert_eq!(
            extra_empty_family.validate().unwrap_err(),
            "expected exactly one circuit-family entry, got 2"
        );

        let nonempty_init_or_teardown = V8ProofShape {
            init_and_teardown_proofs: 1,
            ..canonical_security_100_shape()
        };
        assert_eq!(
            nonempty_init_or_teardown.validate().unwrap_err(),
            "expected no init/teardown proofs, got 1"
        );

        let empty_blake2s_entry = V8ProofShape {
            blake2s_delegation_proofs: Some(0),
            ..canonical_security_100_shape()
        };
        assert!(
            empty_blake2s_entry
                .validate()
                .unwrap_err()
                .contains("with one Blake2s proof")
        );
    }

    /// The pre-keygen source tree must contain only explicit sentinels, never a prior app's
    /// apparently canonical identity. After regeneration, this same test checks the installed
    /// app-derived Security100 chain against the runtime derivation.
    #[test]
    fn v8_app_identity_state_is_coherent() {
        if v8_verifier::V8_APP_IDENTITY_REGENERATION_REQUIRED {
            assert_eq!(v8_verifier::V8_APP_END_PARAMS, [0; 8]);
            assert_eq!(v8_verifier::V8_SECURITY100_EXPECTED_CHAIN, [0; 8]);
            assert_eq!(
                v8_verifier::V8_APP_BIN_MD5,
                "00000000000000000000000000000000"
            );
        } else {
            assert_ne!(v8_verifier::V8_APP_END_PARAMS, [0; 8]);
            assert_ne!(v8_verifier::V8_SECURITY100_EXPECTED_CHAIN, [0; 8]);
            assert_eq!(
                v8_verifier::unified_level_data().expected_chain,
                v8_verifier::V8_SECURITY100_EXPECTED_CHAIN,
            );
        }
    }

    /// Smoke test for the V8 unified-layer verifier lane against a real proof produced by the
    /// airbender CLI (at the rev pinned in the root `Cargo.toml`) with `--target recursion-unified`.
    ///
    /// Run manually:
    ///   V8_PROOF_ARTIFACT_JSON=/path/to/proof.json \
    ///     ./scripts/cargo-with-patched-zksync-os.sh v8-proof-smoke -- \
    ///       test --locked -p zksync_os_server --release \
    ///       v8_unified_layer_verifies_cli_proof -- --ignored
    #[test]
    #[ignore = "needs a locally produced V8 proof artifact"]
    fn v8_unified_layer_verifies_cli_proof() {
        let path = std::env::var("V8_PROOF_ARTIFACT_JSON")
            .expect("set V8_PROOF_ARTIFACT_JSON to an airbender CLI proof.json");
        let artifact: serde_json::Value =
            serde_json::from_reader(std::fs::File::open(&path).expect("cannot open proof file"))
                .expect("proof file is not valid JSON");
        // The CLI stores `ProofArtifact { proof: UnrolledProgramProof, .. }`.
        let proof: execution_utils::unrolled::UnrolledProgramProof =
            serde_json::from_value(artifact["proof"].clone())
                .expect("artifact has no deserializable `proof` field");

        v8_verifier::validate_recursion_chain(&proof)
            .expect("V8 proof carries an invalid recursion chain");
        let registers = v8_verifier::verify(&proof)
            .expect("V8 unified-layer proof failed cryptographic verification");
        println!("final register values: {registers:?}");

        let expected_chain = v8_verifier::unified_level_data().expected_chain;
        assert_eq!(
            registers[8..16],
            expected_chain,
            "proof recursion chain is not rooted in the V8 batch program"
        );
    }
}
