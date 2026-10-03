use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

/// Immutable witness words shared across the pipeline, job map and API readers.
#[derive(Clone)]
pub struct WitnessInput(Arc<Vec<u32>>);

impl WitnessInput {
    pub fn as_slice(&self) -> &[u32] {
        self.0.as_slice()
    }

    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl From<Vec<u32>> for WitnessInput {
    fn from(words: Vec<u32>) -> Self {
        Self(Arc::new(words))
    }
}

impl Deref for WitnessInput {
    type Target = [u32];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl AsRef<[u32]> for WitnessInput {
    fn as_ref(&self) -> &[u32] {
        self.as_slice()
    }
}

impl fmt::Debug for WitnessInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_slice().fmt(formatter)
    }
}

// Delegating to the original Vec representation preserves existing JSON and binary formats.
impl Serialize for WitnessInput {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for WitnessInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<u32>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batcher_model::ProverInput;

    #[derive(Serialize)]
    enum LegacyProverInput {
        Real(Vec<u32>),
        Fake,
    }

    #[test]
    fn clones_share_the_moved_vector_until_the_last_owner_drops() {
        let mut words = Vec::with_capacity(4);
        words.extend([3, 5]);
        let original_pointer = words.as_ptr();
        let original_capacity = words.capacity();
        let input = WitnessInput::from(words);
        let allocation = Arc::downgrade(&input.0);
        let clone = input.clone();

        assert_eq!(input.as_slice().as_ptr(), original_pointer);
        assert_eq!(clone.as_slice().as_ptr(), original_pointer);
        assert_eq!(input.capacity(), original_capacity);
        assert_eq!(Arc::strong_count(&input.0), 2);
        drop(input);
        assert!(allocation.upgrade().is_some());
        assert_eq!(clone.as_slice(), [3, 5]);
        assert_eq!(Arc::strong_count(&clone.0), 1);
        drop(clone);
        assert!(allocation.upgrade().is_none());
    }

    #[test]
    fn json_preserves_word_vectors_and_prover_input_enum_tags() {
        for words in [vec![], vec![0, 1, u32::MAX]] {
            let input = WitnessInput::from(words.clone());
            let vector_bytes = serde_json::to_vec(&words).unwrap();
            assert_eq!(serde_json::to_vec(&input).unwrap(), vector_bytes);
            let decoded: WitnessInput = serde_json::from_slice(&vector_bytes).unwrap();
            assert_eq!(decoded.as_slice(), words);

            let current = ProverInput::Real(input);
            let legacy_bytes = serde_json::to_vec(&LegacyProverInput::Real(words.clone())).unwrap();
            assert_eq!(serde_json::to_vec(&current).unwrap(), legacy_bytes);
            let decoded: ProverInput = serde_json::from_slice(&legacy_bytes).unwrap();
            assert_eq!(decoded.unwrap_real(), words);
        }
        let legacy_bytes = serde_json::to_vec(&LegacyProverInput::Fake).unwrap();
        assert_eq!(
            serde_json::to_vec(&ProverInput::Fake).unwrap(),
            legacy_bytes
        );
        assert!(matches!(
            serde_json::from_slice::<ProverInput>(&legacy_bytes).unwrap(),
            ProverInput::Fake
        ));
    }

    fn assert_bincode_representation<C: bincode::config::Config + Copy>(config: C) {
        for words in [vec![], vec![0, 1, u32::MAX]] {
            let input = WitnessInput::from(words.clone());
            let vector_bytes = bincode::serde::encode_to_vec(&words, config).unwrap();
            assert_eq!(
                bincode::serde::encode_to_vec(&input, config).unwrap(),
                vector_bytes
            );
            let (decoded, consumed): (WitnessInput, usize) =
                bincode::serde::decode_from_slice(&vector_bytes, config).unwrap();
            assert_eq!(consumed, vector_bytes.len());
            assert_eq!(decoded.as_slice(), words);

            let current = ProverInput::Real(input);
            let legacy_bytes =
                bincode::serde::encode_to_vec(LegacyProverInput::Real(words.clone()), config)
                    .unwrap();
            assert_eq!(
                bincode::serde::encode_to_vec(&current, config).unwrap(),
                legacy_bytes
            );
            let (decoded, consumed): (ProverInput, usize) =
                bincode::serde::decode_from_slice(&legacy_bytes, config).unwrap();
            assert_eq!(consumed, legacy_bytes.len());
            assert_eq!(decoded.unwrap_real(), words);
        }
        let legacy_bytes = bincode::serde::encode_to_vec(&LegacyProverInput::Fake, config).unwrap();
        assert_eq!(
            bincode::serde::encode_to_vec(&ProverInput::Fake, config).unwrap(),
            legacy_bytes
        );
        let (decoded, consumed): (ProverInput, usize) =
            bincode::serde::decode_from_slice(&legacy_bytes, config).unwrap();
        assert_eq!(consumed, legacy_bytes.len());
        assert!(matches!(decoded, ProverInput::Fake));
    }

    #[test]
    fn bincode_preserves_word_vectors_and_prover_input_enum_tags() {
        assert_bincode_representation(bincode::config::standard());
        assert_bincode_representation(bincode::config::legacy());
    }
}
