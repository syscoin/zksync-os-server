use alloy::primitives::Bytes;
use alloy::primitives::bytes::BufMut;
use alloy_rlp::{Decodable, Encodable, RlpDecodable, RlpEncodable};

/// SYSCOIN: Exact UTF-8 byte ceiling for diagnostic verifier refusals on the `zks_2fa` wire.
/// Keep producers and the main-node admission check on this single contract.
pub const MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES: usize = 256;

/// SYSCOIN: Bounds a diagnostic refusal without splitting a UTF-8 code point. Refusal text is not
/// consensus data; callers must log sensitive internal detail locally before using a generic reason.
pub fn bounded_verify_batch_refusal_reason(mut reason: String) -> String {
    if reason.len() <= MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES {
        return reason;
    }

    let mut end = MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason.truncate(end);
    reason
}

/// Batch verification request sent by the main node to authenticated verifier peers.
#[derive(Clone, Debug, PartialEq, Eq, Hash, RlpEncodable, RlpDecodable)]
pub struct VerifyBatch {
    pub request_id: u64,
    pub batch_number: u64,
    pub first_block_number: u64,
    pub last_block_number: u64,
    pub pubdata_mode: u8,
    pub commit_data: Bytes,
    pub prev_commit_data: Bytes,
    pub execution_protocol_version: u16,
}

/// Batch verification response sent by a verifier peer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VerifyBatchResult {
    pub request_id: u64,
    pub batch_number: u64,
    pub result: VerifyBatchOutcome,
}

/// Result of verifier-peer processing for a [`VerifyBatch`] request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum VerifyBatchOutcome {
    /// The verifier peer approved the batch and returned its signature bytes.
    Approved(Bytes),
    /// The verifier peer refused the batch and returned a reason string.
    Refused(String),
}

impl Encodable for VerifyBatchResult {
    fn encode(&self, out: &mut dyn BufMut) {
        self.request_id.encode(out);
        self.batch_number.encode(out);
        match &self.result {
            VerifyBatchOutcome::Approved(signature) => {
                0u8.encode(out);
                signature.encode(out);
            }
            VerifyBatchOutcome::Refused(reason) => {
                1u8.encode(out);
                reason.encode(out);
            }
        }
    }

    fn length(&self) -> usize {
        self.request_id.length()
            + self.batch_number.length()
            + 1u8.length()
            + match &self.result {
                VerifyBatchOutcome::Approved(signature) => signature.length(),
                VerifyBatchOutcome::Refused(reason) => reason.length(),
            }
    }
}

impl Decodable for VerifyBatchResult {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let request_id = u64::decode(buf)?;
        let batch_number = u64::decode(buf)?;
        let tag = u8::decode(buf)?;
        let result = match tag {
            0 => VerifyBatchOutcome::Approved(Bytes::decode(buf)?),
            1 => VerifyBatchOutcome::Refused(String::decode(buf)?),
            _ => return Err(alloy_rlp::Error::Custom("invalid verify batch result tag")),
        };
        Ok(Self {
            request_id,
            batch_number,
            result,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES, bounded_verify_batch_refusal_reason};

    // SYSCOIN: The shared wire helper preserves exact-byte reasons and truncates only at a valid
    // UTF-8 boundary when a multibyte code point straddles the 256-byte ceiling.
    #[test]
    fn refusal_reason_bound_is_exact_and_utf8_safe() {
        let exact = "r".repeat(MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES);
        assert_eq!(bounded_verify_batch_refusal_reason(exact.clone()), exact);

        let oversized_ascii = "r".repeat(MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES + 1);
        assert_eq!(
            bounded_verify_batch_refusal_reason(oversized_ascii).len(),
            MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES
        );

        let split_multibyte = format!("{}é", "r".repeat(MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES - 1));
        let bounded = bounded_verify_batch_refusal_reason(split_multibyte);
        assert_eq!(bounded.len(), MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES - 1);
        assert!(bounded.is_char_boundary(bounded.len()));

        let exact_multibyte = format!("{}é", "r".repeat(MAX_VERIFY_BATCH_REFUSAL_REASON_BYTES - 2));
        assert_eq!(
            bounded_verify_batch_refusal_reason(exact_multibyte.clone()),
            exact_multibyte
        );
    }
}
