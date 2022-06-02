use sbor::rust::collections::BTreeSet;
use sbor::rust::vec::Vec;
use sbor::DecodeError;
use scrypto::buffer::scrypto_decode;
use scrypto::engine::types::*;
use scrypto::resource::AuthZoneMethod;
use scrypto::values::ScryptoValue;

use crate::engine::SystemApi;
use crate::model::{Proof, ProofError, ResourceManager};
use crate::wasm::*;

#[derive(Debug, Clone, PartialEq)]
pub enum AuthZoneError {
    EmptyAuthZone,
    ProofError(ProofError),
    CouldNotCreateProof,
    InvalidRequestData(DecodeError),
    CouldNotGetProof,
    CouldNotGetResource,
    NoMethodSpecified,
}

/// A transient resource container.
#[derive(Debug)]
pub struct AuthZone {
    pub proofs: Vec<Proof>,
}

impl AuthZone {
    pub fn new_with_proofs(proofs: Vec<Proof>) -> Self {
        Self { proofs }
    }

    pub fn new() -> Self {
        Self { proofs: Vec::new() }
    }

    fn pop(&mut self) -> Result<Proof, AuthZoneError> {
        if self.proofs.is_empty() {
            return Err(AuthZoneError::EmptyAuthZone);
        }

        Ok(self.proofs.remove(self.proofs.len() - 1))
    }

    pub fn push(&mut self, proof: Proof) {
        self.proofs.push(proof);
    }

    pub fn clear(&mut self) {
        loop {
            if let Some(proof) = self.proofs.pop() {
                proof.drop();
            } else {
                break;
            }
        }
    }

    fn create_proof(
        &self,
        resource_address: ResourceAddress,
        resource_type: ResourceType,
    ) -> Result<Proof, AuthZoneError> {
        Proof::compose(&self.proofs, resource_address, resource_type)
            .map_err(AuthZoneError::ProofError)
    }

    fn create_proof_by_amount(
        &self,
        amount: Decimal,
        resource_address: ResourceAddress,
        resource_type: ResourceType,
    ) -> Result<Proof, AuthZoneError> {
        Proof::compose_by_amount(&self.proofs, amount, resource_address, resource_type)
            .map_err(AuthZoneError::ProofError)
    }

    fn create_proof_by_ids(
        &self,
        ids: &BTreeSet<NonFungibleId>,
        resource_address: ResourceAddress,
        resource_type: ResourceType,
    ) -> Result<Proof, AuthZoneError> {
        Proof::compose_by_ids(&self.proofs, ids, resource_address, resource_type)
            .map_err(AuthZoneError::ProofError)
    }

    pub fn main<S: SystemApi<W, I>, W: WasmEngine<I>, I: WasmInstance>(
        &mut self,
        call_data: ScryptoValue,
        system_api: &mut S,
    ) -> Result<ScryptoValue, AuthZoneError> {
        let method: AuthZoneMethod =
            scrypto_decode(&call_data.raw).map_err(|e| AuthZoneError::InvalidRequestData(e))?;

        match method {
            AuthZoneMethod::Pop() => {
                let proof = self.pop()?;
                let proof_id = system_api
                    .create_proof(proof)
                    .map_err(|_| AuthZoneError::CouldNotCreateProof)?;
                Ok(ScryptoValue::from_value(&scrypto::resource::Proof(
                    proof_id,
                )))
            }
            AuthZoneMethod::Push(proof) => {
                let mut proof = system_api
                    .take_proof(proof.0)
                    .map_err(|_| AuthZoneError::CouldNotGetProof)?;
                // FIXME: this is a hack for now until we can get snode_state into process
                // FIXME: and be able to determine which snode the proof is going into
                proof.change_to_unrestricted();

                self.push(proof);
                Ok(ScryptoValue::from_value(&()))
            }
            AuthZoneMethod::CreateProof(resource_address) => {
                let resource_manager: ResourceManager = system_api
                    .borrow_global_mut_resource_manager(resource_address)
                    .map_err(|_| AuthZoneError::CouldNotGetResource)?;
                let resource_type = resource_manager.resource_type();
                system_api
                    .return_borrowed_global_resource_manager(resource_address, resource_manager);
                let proof = self.create_proof(resource_address, resource_type)?;
                let proof_id = system_api
                    .create_proof(proof)
                    .map_err(|_| AuthZoneError::CouldNotCreateProof)?;
                Ok(ScryptoValue::from_value(&scrypto::resource::Proof(
                    proof_id,
                )))
            }
            AuthZoneMethod::CreateProofByAmount(amount, resource_address) => {
                let resource_manager: ResourceManager = system_api
                    .borrow_global_mut_resource_manager(resource_address)
                    .map_err(|_| AuthZoneError::CouldNotGetResource)?;
                let resource_type = resource_manager.resource_type();
                system_api
                    .return_borrowed_global_resource_manager(resource_address, resource_manager);
                let proof = self.create_proof_by_amount(amount, resource_address, resource_type)?;
                let proof_id = system_api
                    .create_proof(proof)
                    .map_err(|_| AuthZoneError::CouldNotCreateProof)?;
                Ok(ScryptoValue::from_value(&scrypto::resource::Proof(
                    proof_id,
                )))
            }
            AuthZoneMethod::CreateProofByIds(ids, resource_address) => {
                let resource_manager: ResourceManager = system_api
                    .borrow_global_mut_resource_manager(resource_address)
                    .map_err(|_| AuthZoneError::CouldNotGetResource)?;
                let resource_type = resource_manager.resource_type();
                system_api
                    .return_borrowed_global_resource_manager(resource_address, resource_manager);
                let proof = self.create_proof_by_ids(&ids, resource_address, resource_type)?;
                let proof_id = system_api
                    .create_proof(proof)
                    .map_err(|_| AuthZoneError::CouldNotCreateProof)?;
                Ok(ScryptoValue::from_value(&scrypto::resource::Proof(
                    proof_id,
                )))
            }
            AuthZoneMethod::Clear() => {
                self.clear();
                Ok(ScryptoValue::from_value(&()))
            }
        }
    }
}
