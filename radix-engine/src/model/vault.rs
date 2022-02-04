use sbor::*;
use scrypto::rust::vec::Vec;
use scrypto::types::*;

use crate::model::{Actor, Bucket, BucketError, Supply};

/// Represents an error when accessing a vault.
#[derive(Debug, Clone)]
pub enum VaultError {
    AccountingError(BucketError),
    UnauthorizedAccess,
}

/// A persistent resource container on ledger state.
#[derive(Debug, Clone, TypeId, Encode, Decode)]
pub struct Vault {
    bucket: Bucket,
    authority: Address,
}

impl Vault {
    pub fn new(bucket: Bucket, authority: Address) -> Self {
        Self { bucket, authority }
    }

    pub fn put(&mut self, other: Bucket, actor: Actor) -> Result<(), VaultError> {
        if actor.check(self.authority) {
            self.bucket.put(other).map_err(VaultError::AccountingError)
        } else {
            Err(VaultError::UnauthorizedAccess)
        }
    }

    pub fn take(&mut self, amount: Decimal, actor: Actor) -> Result<Bucket, VaultError> {
        if actor.check(self.authority) {
            self.bucket
                .take(amount)
                .map_err(VaultError::AccountingError)
        } else {
            Err(VaultError::UnauthorizedAccess)
        }
    }

    pub fn take_non_fungible(&mut self, key: &NonFungibleKey, actor: Actor) -> Result<Bucket, VaultError> {
        if actor.check(self.authority) {
            self.bucket
                .take_non_fungible(key)
                .map_err(VaultError::AccountingError)
        } else {
            Err(VaultError::UnauthorizedAccess)
        }
    }

    pub fn get_non_fungible_ids(&self, actor: Actor) -> Result<Vec<NonFungibleKey>, VaultError> {
        if actor.check(self.authority) {
            self.bucket
                .get_non_fungible_keys()
                .map_err(VaultError::AccountingError)
        } else {
            Err(VaultError::UnauthorizedAccess)
        }
    }

    pub fn total_supply(&self, actor: Actor) -> Result<Supply, VaultError> {
        if actor.check(self.authority) {
            Ok(self.bucket.supply())
        } else {
            Err(VaultError::UnauthorizedAccess)
        }
    }

    pub fn amount(&self, actor: Actor) -> Result<Decimal, VaultError> {
        if actor.check(self.authority) {
            Ok(self.bucket.amount())
        } else {
            Err(VaultError::UnauthorizedAccess)
        }
    }

    pub fn resource_address(&self, actor: Actor) -> Result<Address, VaultError> {
        if actor.check(self.authority) {
            Ok(self.bucket.resource_address())
        } else {
            Err(VaultError::UnauthorizedAccess)
        }
    }
}
