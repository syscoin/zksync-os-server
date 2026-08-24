use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use zksync_os_types::ProvingVersion;

// SYSCOIN: Bound diagnostic storage and reject control/query-delimiter characters before a
// request reaches assignment state or logs. Metrics use a separate fixed-cardinality label.
const MAX_PROVER_ID_BYTES: usize = 64;

fn deserialize_prover_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let id = String::deserialize(deserializer)?;
    let valid = !id.is_empty()
        && id.len() <= MAX_PROVER_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if !valid {
        return Err(D::Error::custom(
            "prover id must be 1-64 ASCII alphanumeric, '-', '_', '.', or ':' characters",
        ));
    }
    Ok(id)
}

#[derive(Serialize, Deserialize)]
pub(super) struct BatchDataPayload {
    pub batch_number: u64,
    pub vk_hash: String,
    pub prover_input: String, // base64‑encoded little‑endian u32 array
    // SYSCOIN: Opaque pick capability; deliberately absent from peek and status payloads.
    pub lease_token: String,
}

// SYSCOIN: Read-only FRI material is structurally incapable of carrying a pick capability.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PeekBatchDataPayload {
    pub batch_number: u64,
    pub vk_hash: String,
    pub prover_input: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ProverQuery {
    #[serde(deserialize_with = "deserialize_prover_id")]
    pub id: String,
    /// Comma-separated vk_hashes of the proving versions this prover supports.
    #[serde(default)]
    pub supported_vk_hashes: Option<String>,
}

impl ProverQuery {
    /// Proving versions this prover declared support for.
    ///
    /// `None` means no declaration and the caller must not filter jobs. This is a
    /// backwards-compatibility layer: old provers don't send `supported_vk_hashes`
    /// and must keep receiving jobs as before.
    ///
    /// Provers currently declare exactly one version; the list shape is for
    /// multi-version provers, which become possible on the prover side with
    /// airbender v2.
    ///
    /// A declared hash the server doesn't recognize is skipped with a warning: it's
    /// most likely a proving version newer than this server, and the prover should
    /// still be served the versions both sides know. If nothing in the declaration
    /// is recognized, this returns `Some(vec![])` so that such a prover gets *no*
    /// jobs rather than any jobs.
    pub fn supported_proving_versions(&self) -> Option<Vec<ProvingVersion>> {
        let hashes: Vec<&str> = self
            .supported_vk_hashes
            .iter()
            .flat_map(|hashes| hashes.split(','))
            .map(str::trim)
            .filter(|hash| !hash.is_empty())
            .collect();

        if hashes.is_empty() {
            return None;
        }

        let versions = hashes
            .into_iter()
            .filter_map(|hash| match ProvingVersion::try_from_vk_hash(hash) {
                Ok(version) => Some(version),
                Err(_) => {
                    tracing::warn!(
                        prover_id = self.id,
                        vk_hash = hash,
                        "prover declared a vk_hash unknown to this server; ignoring it"
                    );
                    None
                }
            })
            .collect();

        Some(versions)
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct FriProofPayload {
    pub batch_number: u64,
    pub vk_hash: String,
    pub proof: String,
    // SYSCOIN: Prover ID is display metadata; this random capability authorizes submission.
    pub lease_token: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct NextSnarkProverJobPayload {
    pub from_batch_number: u64,
    pub to_batch_number: u64,
    pub vk_hash: String,
    pub fri_proofs: Vec<String>, // base64‑encoded FRI proofs (little‑endian u32 array)
    // SYSCOIN: One capability covers exactly this immutable aggregate range.
    pub lease_token: String,
}

// SYSCOIN: Read-only aggregate material is structurally incapable of carrying a lease.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct PeekSnarkProverJobPayload {
    pub from_batch_number: u64,
    pub to_batch_number: u64,
    pub vk_hash: String,
    pub fri_proofs: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub(super) struct SnarkProofPayload {
    pub from_batch_number: u64,
    pub to_batch_number: u64,
    pub vk_hash: String,
    pub proof: String,
    // SYSCOIN: Exact-range completion requires the capability returned by SNARK pick.
    pub lease_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct FailedProofResponse {
    pub batch_number: u64,
    pub last_batch_timestamp: u64,
    pub expected_hash_u32s: [u32; 8],
    pub proof_final_register_values: [u32; 16],
    pub vk_hash: String,
    pub proof: String, // base64‑encoded FRI proof (little‑endian u32 array)
}

#[cfg(test)]
mod tests {
    use super::ProverQuery;
    use serde_json::json;
    use zksync_os_types::ProvingVersion;

    const UNKNOWN_VK_HASH: &str =
        "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    #[test]
    fn prover_id_is_bounded_before_observability_or_assignment() {
        for id in ["fri-0", "rack_1.node:2", "A9"] {
            let query: ProverQuery = serde_json::from_value(json!({ "id": id })).unwrap();
            assert_eq!(query.id, id);
        }
        for id in [
            "",
            "contains space",
            "log\ninjection",
            "query&injection",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(serde_json::from_value::<ProverQuery>(json!({ "id": id })).is_err());
        }
    }

    fn query(supported_vk_hashes: Option<&str>) -> ProverQuery {
        ProverQuery {
            id: "test_prover".to_string(),
            supported_vk_hashes: supported_vk_hashes.map(str::to_string),
        }
    }

    #[test]
    fn no_declaration_means_no_filter() {
        assert_eq!(query(None).supported_proving_versions(), None);
        // Declared-but-blank is treated the same as absent
        assert_eq!(query(Some("")).supported_proving_versions(), None);
        assert_eq!(query(Some(" ,, ")).supported_proving_versions(), None);
    }

    #[test]
    // SYSCOIN: Fresh V32 remote provers advertise only the canonical V8 verification key.
    fn known_hashes_are_parsed() {
        let q = query(Some(ProvingVersion::V8.vk_hash()));
        assert_eq!(
            q.supported_proving_versions(),
            Some(vec![ProvingVersion::V8])
        );
    }

    #[test]
    fn unknown_hash_is_skipped_keeping_known_ones() {
        let q = query(Some(&format!(
            "{},{}",
            UNKNOWN_VK_HASH,
            ProvingVersion::V8.vk_hash()
        )));
        assert_eq!(
            q.supported_proving_versions(),
            Some(vec![ProvingVersion::V8])
        );
    }

    #[test]
    fn all_unknown_hashes_mean_no_jobs_not_no_filter() {
        let q = query(Some(UNKNOWN_VK_HASH));
        assert_eq!(q.supported_proving_versions(), Some(vec![]));
    }
}
