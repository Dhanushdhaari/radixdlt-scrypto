use sbor::Encode;
use scrypto::buffer::scrypto_encode;
use scrypto::rust::collections::HashMap;
use scrypto::rust::vec::Vec;

use crate::ledger::traits::{Substate, WriteableSubstateStore};
use crate::ledger::*;

/// An in-memory ledger stores all substates in host memory.
#[derive(Debug, Clone)]
pub struct InMemorySubstateStore {
    substates: HashMap<Vec<u8>, Substate>,
    current_epoch: u64,
    nonce: u64,
}

impl InMemorySubstateStore {
    pub fn new() -> Self {
        Self {
            substates: HashMap::new(),
            current_epoch: 0,
            nonce: 0,
        }
    }

    pub fn with_bootstrap() -> Self {
        let mut ledger = Self::new();
        bootstrap(&mut ledger);
        ledger
    }
}

impl Default for InMemorySubstateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadableSubstateStore for InMemorySubstateStore {
    fn get_substate<T: Encode>(&self, address: &T) -> Option<Substate> {
        self.substates.get(&scrypto_encode(address)).cloned()
    }

    fn get_child_substate<T: Encode>(&self, address: &T, key: &[u8]) -> Option<Substate> {
        let mut id = scrypto_encode(address);
        id.extend(key.to_vec());
        self.substates.get(&id).cloned()
    }


    fn get_epoch(&self) -> u64 {
        self.current_epoch
    }

    fn get_nonce(&self) -> u64 {
        self.nonce
    }
}

impl WriteableSubstateStore for InMemorySubstateStore {
    fn put_substate(&mut self, address: &[u8], substate: Substate) {
        self.substates.insert(address.to_vec(), substate);
    }

    fn put_child_substate(&mut self, address: &[u8], key: &[u8], substate: Substate) {
        let mut id = address.to_vec();
        id.extend(key.to_vec());
        self.substates.insert(id, substate);
    }

    fn set_epoch(&mut self, epoch: u64) {
        self.current_epoch = epoch;
    }

    fn increase_nonce(&mut self) {
        self.nonce += 1;
    }
}