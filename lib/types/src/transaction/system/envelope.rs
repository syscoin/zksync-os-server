use alloy::consensus::transaction::{RlpEcdsaDecodableTx, RlpEcdsaEncodableTx};
use alloy::consensus::{Transaction, Typed2718};
use alloy::eips::eip2718::{Eip2718Error, Eip2718Result};
use alloy::eips::{Decodable2718, Encodable2718};
use alloy::primitives::ChainId;
use alloy::primitives::{B256, Bytes, TxHash, TxKind, U256};
use alloy::rpc::types::{AccessList, SignedAuthorization};
use alloy::signers::Signature;
use alloy_rlp::{BufMut, Encodable};
use serde::{Deserialize, Serialize};

use crate::transaction::SystemTxType;
use crate::transaction::system::tx::SystemTransaction;

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct SystemTransactionEnvelope<T: SystemTxType> {
    pub inner: SystemTransaction<T>,
}

impl<T: SystemTxType> SystemTransactionEnvelope<T> {
    pub fn hash(&self) -> &B256 {
        todo!("not implemented")
    }
}

impl<T: SystemTxType> Typed2718 for SystemTransactionEnvelope<T> {
    fn ty(&self) -> u8 {
        T::TX_TYPE
    }
}

impl<T: SystemTxType> RlpEcdsaEncodableTx for SystemTransactionEnvelope<T> {
    fn rlp_encoded_fields_length(&self) -> usize {
        self.inner.rlp_encoded_fields_length()
    }

    fn rlp_encode_fields(&self, out: &mut dyn BufMut) {
        self.inner.rlp_encode_fields(out);
    }

    fn tx_hash_with_type(&self, _signature: &Signature, _ty: u8) -> TxHash {
        todo!("not implemented")
    }
}

impl<T: SystemTxType> RlpEcdsaDecodableTx for SystemTransactionEnvelope<T> {
    const DEFAULT_TX_TYPE: u8 = T::TX_TYPE;

    fn rlp_decode_fields(buf: &mut &[u8]) -> alloy::rlp::Result<Self> {
        Ok(Self {
            inner: SystemTransaction::rlp_decode_fields(buf)?,
        })
    }
}

impl<T: SystemTxType> Encodable for SystemTransactionEnvelope<T> {
    fn encode(&self, out: &mut dyn BufMut) {
        self.inner.encode(out);
    }

    fn length(&self) -> usize {
        self.inner.length()
    }
}

impl<T: SystemTxType> Encodable2718 for SystemTransactionEnvelope<T> {
    fn encode_2718_len(&self) -> usize {
        self.inner
            .eip2718_encoded_length(&Signature::new(U256::ZERO, U256::ZERO, false))
    }

    fn encode_2718(&self, out: &mut dyn BufMut) {
        self.inner
            .eip2718_encode(&Signature::new(U256::ZERO, U256::ZERO, false), out)
    }
}

impl<T: SystemTxType> Decodable2718 for SystemTransactionEnvelope<T> {
    fn typed_decode(ty: u8, buf: &mut &[u8]) -> Eip2718Result<Self> {
        let decoded = SystemTransaction::rlp_decode_signed(buf)?;

        if decoded.ty() != ty {
            return Err(Eip2718Error::UnexpectedType(ty));
        }

        Ok(Self {
            inner: decoded.into_parts().0,
        })
    }

    fn fallback_decode(_buf: &mut &[u8]) -> Eip2718Result<Self> {
        // Do not try to decode untyped transactions
        Err(Eip2718Error::UnexpectedType(0))
    }
}

impl<T: SystemTxType> Transaction for SystemTransactionEnvelope<T> {
    fn chain_id(&self) -> Option<ChainId> {
        self.inner.chain_id()
    }

    fn nonce(&self) -> u64 {
        self.inner.nonce()
    }

    fn gas_limit(&self) -> u64 {
        self.inner.gas_limit()
    }

    fn gas_price(&self) -> Option<u128> {
        self.inner.gas_price()
    }

    fn max_fee_per_gas(&self) -> u128 {
        self.inner.max_fee_per_gas()
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        self.inner.max_priority_fee_per_gas()
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        self.inner.max_fee_per_blob_gas()
    }

    fn priority_fee_or_price(&self) -> u128 {
        self.inner.priority_fee_or_price()
    }

    fn effective_gas_price(&self, base_fee: Option<u64>) -> u128 {
        self.inner.effective_gas_price(base_fee)
    }

    fn is_dynamic_fee(&self) -> bool {
        self.inner.is_dynamic_fee()
    }

    fn kind(&self) -> TxKind {
        self.inner.kind()
    }

    fn is_create(&self) -> bool {
        self.inner.is_create()
    }

    fn value(&self) -> U256 {
        self.inner.value()
    }

    fn input(&self) -> &Bytes {
        self.inner.input()
    }

    fn access_list(&self) -> Option<&AccessList> {
        self.inner.access_list()
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        self.inner.blob_versioned_hashes()
    }

    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        self.inner.authorization_list()
    }
}
