use sbor::rust::collections::*;
use sbor::rust::vec::Vec;
use sbor::*;
use scrypto::buffer::*;
use scrypto::crypto::*;
use scrypto::engine::types::*;

pub trait QueryableSubstateStore {
    fn get_lazy_map_entries(
        &self,
        component_address: ComponentAddress,
        lazy_map_id: &LazyMapId,
    ) -> HashMap<Vec<u8>, Vec<u8>>;
}

#[derive(Debug, Clone, Hash, TypeId, Encode, Decode, PartialEq, Eq)]
pub struct PhysicalSubstateId(pub Hash, pub u32);

#[derive(Clone, Debug, Encode, Decode, TypeId)]
pub struct Substate {
    pub value: Vec<u8>,
    pub phys_id: PhysicalSubstateId,
}

#[derive(Debug)]
pub struct SubstateIdGenerator {
    tx_hash: Hash,
    count: u32,
}

impl SubstateIdGenerator {
    pub fn new(tx_hash: Hash) -> Self {
        Self { tx_hash, count: 0 }
    }

    pub fn next(&mut self) -> PhysicalSubstateId {
        let value = self.count;
        self.count = self.count + 1;
        PhysicalSubstateId(self.tx_hash.clone(), value)
    }
}

/// A ledger stores all transactions and substates.
pub trait ReadableSubstateStore {
    fn get_substate(&self, address: &[u8]) -> Option<Substate>;
    fn get_space(&mut self, address: &[u8]) -> Option<PhysicalSubstateId>;

    // Temporary Encoded/Decoded interface
    fn get_decoded_substate<A: Encode, T: Decode>(
        &self,
        address: &A,
    ) -> Option<(T, PhysicalSubstateId)> {
        self.get_substate(&scrypto_encode(address))
            .map(|s| (scrypto_decode(&s.value).unwrap(), s.phys_id))
    }

    fn get_epoch(&self) -> u64;

    // TODO: redefine what nonce is and how it's updated
    // For now, we bump nonce only when a transaction has been committed
    // or when an account is created (for testing).
    fn get_nonce(&self) -> u64;
}

pub trait WriteableSubstateStore {
    fn put_substate(&mut self, address: &[u8], substate: Substate);
    fn put_space(&mut self, address: &[u8], phys_id: PhysicalSubstateId);
    fn set_epoch(&mut self, epoch: u64);
    fn increase_nonce(&mut self);
}
