//! Differential tests for the SLH-DSA-SHA2-128-24 precompile (0x101).
//!
//! Runs the exact same known-answer vector and deterministic mutation sweep as
//! the Solidity fallback verifier test in
//! `contracts/test/SLHDSASHA212824Differential.t.sol`. Both sides must accept
//! the valid vector and reject every mutated/random vector, detecting
//! divergence on this finite corpus. This is regression coverage, not
//! coverage-guided fuzzing or a general equivalence proof.
//!
//! Keep `MUTATION_MASKS`, `SIG_BOUNDARY_OFFSETS`, `MUTATION_STRIDE`,
//! `RANDOM_VECTORS`, and `RANDOM_SEED` in sync with the Solidity test.
#![feature(allocator_api)]

use basic_system::system_functions::slh_dsa_sha2_128_24_verify::SlhDsaSha212824VerifyImpl;
use serde::Deserialize;
use std::alloc::Global;
use zk_ee::reference_implementations::{BaseResources, DecreasingNative};
use zk_ee::system::{Resource, SystemFunction};

const SIGNATURE_LEN: usize = 3856;
/// Word-aligned input layout: pkSeed(32) || pkRoot(32) || message(32) || sig.
const SIG_OFFSET: usize = 96;

const MUTATION_MASKS: [u8; 2] = [0x01, 0x80];
/// Signature component boundaries: R randomizer [0..16), FORS trees
/// [16..2416) (6 trees x (16-byte sk + 24 x 16-byte auth path)), WOTS chains
/// [2416..3504) (68 x 16), Merkle auth path [3504..3856) (22 x 16).
const SIG_BOUNDARY_OFFSETS: [usize; 14] =
    [0, 15, 16, 31, 32, 415, 416, 2415, 2416, 2431, 3503, 3504, 3519, 3855];
const MUTATION_STRIDE: usize = 31;
const RANDOM_VECTORS: usize = 8;
const RANDOM_SEED: u64 = 0x5EED_5EED_5EED_5EED;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KatVector {
    pk_seed: String,
    pk_root: String,
    message: String,
    signature: String,
    expected: bool,
}

fn kat_input() -> Vec<u8> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/test/vectors/slh_dsa_sha2_128_24_kat.json"
    );
    let raw = std::fs::read_to_string(path).expect("shared KAT vector file");
    let vector: KatVector = serde_json::from_str(&raw).expect("valid KAT JSON");
    assert!(vector.expected, "KAT fixture must be a valid signature");

    let decode = |field: &str| hex::decode(field.trim_start_matches("0x")).expect("valid hex");
    let (pk_seed, pk_root, message, signature) = (
        decode(&vector.pk_seed),
        decode(&vector.pk_root),
        decode(&vector.message),
        decode(&vector.signature),
    );
    assert_eq!(pk_seed.len(), 32);
    assert_eq!(pk_root.len(), 32);
    assert_eq!(message.len(), 32);
    assert_eq!(signature.len(), SIGNATURE_LEN);

    let mut input = Vec::with_capacity(SIG_OFFSET + SIGNATURE_LEN);
    input.extend_from_slice(&pk_seed);
    input.extend_from_slice(&pk_root);
    input.extend_from_slice(&message);
    input.extend_from_slice(&signature);
    input
}

/// Executes the precompile implementation and returns whether it accepted.
fn verify(input: &[u8]) -> bool {
    let mut dst = vec![];
    let mut resources = BaseResources::<DecreasingNative>::FORMAL_INFINITE;
    SlhDsaSha212824VerifyImpl::execute(input, &mut dst, &mut resources, Global)
        .expect("precompile execution");
    let mut expected_one = [0u8; 32];
    expected_one[31] = 1;
    if dst == expected_one {
        true
    } else {
        assert_eq!(dst, [0u8; 32], "precompile returned non-boolean output");
        false
    }
}

/// xorshift64* PRNG; identical byte stream in the Solidity test.
fn xorshift64star(state: &mut u64) -> u8 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
}

#[test]
fn valid_kat_verifies() {
    assert!(verify(&kat_input()), "valid KAT rejected");
}

#[test]
fn mutated_signature_rejects() {
    let input = kat_input();
    let mut offsets: Vec<usize> = SIG_BOUNDARY_OFFSETS.to_vec();
    offsets.extend((0..SIGNATURE_LEN).step_by(MUTATION_STRIDE));
    for offset in offsets {
        for mask in MUTATION_MASKS {
            let mut mutated = input.clone();
            mutated[SIG_OFFSET + offset] ^= mask;
            assert!(
                !verify(&mutated),
                "sig mutation accepted at offset {offset} mask {mask:#04x}"
            );
        }
    }
}

#[test]
fn mutated_seed_root_message_rejects() {
    let input = kat_input();
    // First/last meaningful byte of pkSeed, pkRoot, and the message.
    for offset in [0usize, 15, 32, 47, 64, 95] {
        for mask in MUTATION_MASKS {
            let mut mutated = input.clone();
            mutated[offset] ^= mask;
            assert!(
                !verify(&mutated),
                "header mutation accepted at offset {offset} mask {mask:#04x}"
            );
        }
    }
}

#[test]
fn noncanonical_key_padding_rejects() {
    let input = kat_input();
    // Nonzero low 16 bytes of the pkSeed/pkRoot words must be rejected.
    // (The Solidity verifier reverts with "Invalid public key" here; the
    // precompile signals failure by returning 0.)
    for offset in [16usize, 31, 48, 63] {
        let mut mutated = input.clone();
        mutated[offset] = 1;
        assert!(!verify(&mutated), "noncanonical key accepted at offset {offset}");
    }
}

#[test]
fn wrong_length_rejects() {
    let input = kat_input();
    // (The Solidity verifier reverts with "Invalid sig length" here; the
    // precompile signals failure by returning 0.)
    assert!(!verify(&input[..input.len() - 1]), "truncated input accepted");
    let mut extended = input.clone();
    extended.push(0);
    assert!(!verify(&extended), "extended input accepted");
    assert!(!verify(&[]), "empty input accepted");
}

#[test]
fn random_signatures_reject() {
    let input = kat_input();
    let mut state = RANDOM_SEED;
    for vector in 0..RANDOM_VECTORS {
        let mut mutated = input.clone();
        for byte in &mut mutated[SIG_OFFSET..] {
            *byte = xorshift64star(&mut state);
        }
        assert!(!verify(&mutated), "random signature accepted, vector {vector}");
    }
}
