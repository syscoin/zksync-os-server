pub mod fake_fri_provers_pool;
pub mod fri_job_manager;
mod fri_proof_verifier;
pub mod fri_proving_pipeline_step;
pub mod gapless_committer;
pub mod gapless_l1_proof_sender;
mod metrics;
pub mod proof_storage;
mod prover_job_map;
pub mod prover_server;
pub mod snark_job_manager;
// SYSCOIN: Verify real wrapper proofs against one canonical settlement-layer snapshot before any
// durable local acceptance or job consumption.
pub(crate) mod snark_proof_preflight;
// SYSCOIN: Keep accepted wrapper proofs crash-safe until their validated L1 receipt is confirmed.
pub(crate) mod snark_proof_journal;
pub mod snark_proving_pipeline_step;
#[cfg(test)]
mod test_util;

use zksync_os_batch_types::batcher_model::ProverInput;

// SYSCOIN: Clamp a remote worker's advertised response capacity to the current production proxy
// spool / 64-GiB host budget. This is a deployment capacity gate, not a canonical V8 witness
// bound: larger jobs remain unleased until worker, node, and proxy capacities are raised together.
pub(crate) const MAX_FRI_PICK_RESPONSE_BYTES: usize = 384 * 1024 * 1024;
pub(crate) const MAX_FRI_PEEK_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
// SYSCOIN: Covers field names, maximal u64 batch number, two canonical B256 strings, quotes, and
// JSON punctuation. A handler test keeps this conservative allowance above actual framing.
pub(crate) const FRI_PICK_RESPONSE_FRAMING_BYTES: usize = 512;

pub(crate) fn fri_input_words_fit_response_contract(
    word_count: usize,
    maximum_response_bytes: usize,
) -> bool {
    word_count
        .checked_mul(std::mem::size_of::<u32>())
        .and_then(|bytes| bytes.checked_add(2))
        .and_then(|bytes| bytes.checked_div(3))
        .and_then(|bytes| bytes.checked_mul(4))
        .and_then(|bytes| bytes.checked_add(FRI_PICK_RESPONSE_FRAMING_BYTES))
        .is_some_and(|bytes| bytes <= maximum_response_bytes)
}

pub(crate) fn fri_input_fits_response_contract(
    input: &ProverInput,
    maximum_response_bytes: usize,
) -> bool {
    let word_count = match input {
        ProverInput::Real(words) => words.len(),
        ProverInput::Fake => 0,
    };
    fri_input_words_fit_response_contract(word_count, maximum_response_bytes)
}

#[cfg(test)]
mod response_contract_tests {
    use super::*;

    #[test]
    fn fri_response_word_bound_is_exact_and_overflow_safe() {
        let maximum = MAX_FRI_PICK_RESPONSE_BYTES;
        let mut last_fitting = (maximum - FRI_PICK_RESPONSE_FRAMING_BYTES).saturating_mul(3)
            / 4
            / std::mem::size_of::<u32>();
        while !fri_input_words_fit_response_contract(last_fitting, maximum) {
            last_fitting -= 1;
        }
        while fri_input_words_fit_response_contract(last_fitting + 1, maximum) {
            last_fitting += 1;
        }
        assert!(fri_input_words_fit_response_contract(last_fitting, maximum));
        assert!(!fri_input_words_fit_response_contract(
            last_fitting + 1,
            maximum
        ));
        assert!(!fri_input_words_fit_response_contract(usize::MAX, maximum));
    }

    #[test]
    fn known_large_v8_witness_is_not_confused_with_diagnostic_capacity() {
        // SYSCOIN: 23,608 depth-64 storage proofs record 533 u32 words each. Their base64 body
        // exceeds 64 MiB but fits the production pick lane, so filtering happens by worker
        // capacity rather than a protocol-invalidating global ceiling.
        let witness_words = 23_608 * 533;
        assert!(!fri_input_words_fit_response_contract(
            witness_words,
            MAX_FRI_PEEK_RESPONSE_BYTES
        ));
        assert!(fri_input_words_fit_response_contract(
            witness_words,
            MAX_FRI_PICK_RESPONSE_BYTES
        ));
    }
}
