use alloy::consensus::transaction::{RlpEcdsaDecodableTx, RlpEcdsaEncodableTx};
use alloy::consensus::{Transaction, Typed2718};
use alloy::eips::Encodable2718;
use alloy::primitives::ChainId;
use alloy::primitives::{Address, B256, Bytes, TxHash, TxKind, U256};
use alloy::rpc::types::{AccessList, SignedAuthorization};
use alloy::signers::Signature;
use alloy_rlp::{BufMut, Decodable, Encodable};
use serde::{Deserialize, Serialize};

use crate::transaction::SystemTxType;

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct SystemTransaction<T: SystemTxType> {
    pub initiator: Address,
    pub gas_limit: u64,
    pub destination: Address,
    pub data: Bytes,

    #[serde(skip)]
    pub marker: std::marker::PhantomData<T>,
}

impl<T: SystemTxType> Transaction for SystemTransaction<T> {
    fn chain_id(&self) -> Option<ChainId> {
        None
    }

    fn nonce(&self) -> u64 {
        // todo: check if this is correct
        0
    }

    fn gas_limit(&self) -> u64 {
        self.gas_limit
    }

    fn gas_price(&self) -> Option<u128> {
        None
    }

    fn max_fee_per_gas(&self) -> u128 {
        // todo: check if this is correct
        0
    }

    fn max_priority_fee_per_gas(&self) -> Option<u128> {
        // todo: check if this is correct
        Some(0)
    }

    fn max_fee_per_blob_gas(&self) -> Option<u128> {
        None
    }

    fn priority_fee_or_price(&self) -> u128 {
        // todo: check if this is correct
        0
    }

    fn effective_gas_price(&self, _base_fee: Option<u64>) -> u128 {
        // At the moment `max_fee_per_gas` is the effective gas price for L1 txs.
        // todo: check if this is correct
        0
    }

    fn is_dynamic_fee(&self) -> bool {
        true
    }

    fn kind(&self) -> TxKind {
        // todo: check if this is correct
        TxKind::Call(self.destination)
    }

    fn is_create(&self) -> bool {
        false
    }

    fn value(&self) -> U256 {
        // todo: check if this is correct
        U256::ZERO
    }

    fn input(&self) -> &Bytes {
        &self.data
    }

    fn access_list(&self) -> Option<&AccessList> {
        None
    }

    fn blob_versioned_hashes(&self) -> Option<&[B256]> {
        None
    }

    fn authorization_list(&self) -> Option<&[SignedAuthorization]> {
        None
    }
}

impl<T: SystemTxType> Typed2718 for SystemTransaction<T> {
    fn ty(&self) -> u8 {
        T::TX_TYPE
    }
}

impl<T: SystemTxType> RlpEcdsaEncodableTx for SystemTransaction<T> {
    fn rlp_encoded_fields_length(&self) -> usize {
        self.gas_limit.length() + self.destination.length() + self.data.length()
    }

    fn rlp_encode_fields(&self, out: &mut dyn BufMut) {
        self.gas_limit.encode(out);
        self.destination.encode(out);
        self.data.encode(out);
    }

    fn tx_hash_with_type(&self, _signature: &Signature, _ty: u8) -> TxHash {
        todo!("not implemented")
    }
}

impl<T: SystemTxType> RlpEcdsaDecodableTx for SystemTransaction<T> {
    const DEFAULT_TX_TYPE: u8 = T::TX_TYPE;

    fn rlp_decode_fields(buf: &mut &[u8]) -> alloy::rlp::Result<Self> {
        Ok(Self {
            initiator: Decodable::decode(buf)?,
            gas_limit: Decodable::decode(buf)?,
            destination: Decodable::decode(buf)?,
            data: Decodable::decode(buf)?,

            marker: std::marker::PhantomData,
        })
    }
}

impl<T: SystemTxType> Encodable for SystemTransaction<T> {
    fn encode(&self, out: &mut dyn BufMut) {
        self.rlp_encode(out);
    }

    fn length(&self) -> usize {
        self.rlp_encoded_length()
    }
}

impl<T: SystemTxType> Encodable2718 for SystemTransaction<T> {
    fn encode_2718_len(&self) -> usize {
        self.eip2718_encoded_length(&Signature::new(U256::ZERO, U256::ZERO, false))
    }

    fn encode_2718(&self, out: &mut dyn BufMut) {
        self.eip2718_encode(&Signature::new(U256::ZERO, U256::ZERO, false), out)
    }
}
