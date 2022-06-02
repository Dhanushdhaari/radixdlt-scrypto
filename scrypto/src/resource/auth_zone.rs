use sbor::rust::collections::BTreeSet;
use sbor::rust::string::ToString;
use sbor::*;

use crate::buffer::scrypto_decode;
use crate::core::SNodeRef;
use crate::engine::{api::*, call_engine};
use crate::math::Decimal;
use crate::resource::*;
use crate::sfunctions;

#[derive(Debug, TypeId, Encode, Decode)]
pub enum AuthZoneMethod {
    Push(Proof),
    Pop(),
    Clear(),
    CreateProof(ResourceAddress),
    CreateProofByAmount(Decimal, ResourceAddress),
    CreateProofByIds(BTreeSet<NonFungibleId>, ResourceAddress),
}

/// Represents the auth zone, which is used by system for checking
/// if this component is allowed to
///
/// 1. Call methods on another component;
/// 2. Access resource system.
pub struct ComponentAuthZone {}

impl ComponentAuthZone {
    sfunctions! {
        SNodeRef::AuthZoneRef => {
            pub fn push(proof: Proof) -> () {
                AuthZoneMethod::Push(proof)
            }

            pub fn pop() -> Proof {
                AuthZoneMethod::Pop()
            }

            pub fn create_proof(resource_address: ResourceAddress) -> Proof {
                AuthZoneMethod::CreateProof(resource_address)
            }

            pub fn create_proof_by_amount(amount: Decimal, resource_address: ResourceAddress) -> Proof {
                AuthZoneMethod::CreateProofByAmount(amount, resource_address)
            }

            pub fn create_proof_by_ids(ids: &BTreeSet<NonFungibleId>, resource_address: ResourceAddress) -> Proof {
                AuthZoneMethod::CreateProofByIds(ids.clone(), resource_address)
            }
        }
    }
}

#[derive(Debug, TypeId, Encode, Decode)]
pub enum CallerAuthZoneMethod {
    CreateProof(ResourceAddress),
    CreateProofByAmount(Decimal, ResourceAddress),
    CreateProofByIds(BTreeSet<NonFungibleId>, ResourceAddress),
}

// just like above for create_proof* but targeting CallAuthZoneRef instead of AuthZoneRef
pub struct CallerAuthZone {}

impl CallerAuthZone {
    sfunctions! {
        SNodeRef::CallerAuthZoneRef => {
            pub fn create_proof(resource_address: ResourceAddress) -> Proof {
                CallerAuthZoneMethod::CreateProof(resource_address)
            }

            pub fn create_proof_by_amount(amount: Decimal, resource_address: ResourceAddress) -> Proof {
                CallerAuthZoneMethod::CreateProofByAmount(amount, resource_address)
            }

            pub fn create_proof_by_ids(ids: &BTreeSet<NonFungibleId>, resource_address: ResourceAddress) -> Proof {
                CallerAuthZoneMethod::CreateProofByIds(ids.clone(), resource_address)
            }
        }
    }
}
