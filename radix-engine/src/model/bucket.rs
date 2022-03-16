use scrypto::engine::types::*;
use scrypto::prelude::NonFungibleAddress;
use scrypto::rust::collections::BTreeSet;

use crate::model::{ResourceContainer, ResourceContainerError};

/// Represents an error when accessing a bucket.
#[derive(Debug, Clone, PartialEq)]
pub enum BucketError {
    ResourceContainerError(ResourceContainerError),
    BucketLocked,
    OtherBucketLocked,
}

/// A transient resource container.
#[derive(Debug)]
pub struct Bucket {
    container: ResourceContainer,
}

impl Bucket {
    pub fn new(container: ResourceContainer) -> Self {
        Self { container }
    }

    pub fn put(&mut self, other: Bucket) -> Result<(), BucketError> {
        self.container
            .put(other.take_container())
            .map_err(BucketError::ResourceContainerError)
    }

    pub fn take(&mut self, amount: Decimal) -> Result<Bucket, BucketError> {
        Ok(Bucket::new(
            self.container
                .take(amount)
                .map_err(BucketError::ResourceContainerError)?,
        ))
    }

    pub fn take_non_fungible(&mut self, id: &NonFungibleId) -> Result<Bucket, BucketError> {
        self.take_non_fungibles(&BTreeSet::from([id.clone()]))
    }

    pub fn take_non_fungibles(
        &mut self,
        ids: &BTreeSet<NonFungibleId>,
    ) -> Result<Bucket, BucketError> {
        Ok(Bucket::new(
            self.container
                .take_non_fungibles(ids)
                .map_err(BucketError::ResourceContainerError)?,
        ))
    }

    pub fn contains_non_fungible_address(&self, non_fungible_address: &NonFungibleAddress) -> bool {
        if self.resource_def_id() != non_fungible_address.resource_def_id() {
            return false;
        }

        match self.container.liquid_amount().as_non_fungible_ids() {
            Err(_) => false,
            Ok(non_fungible_ids) => non_fungible_ids
                .iter()
                .any(|k| k.eq(&non_fungible_address.non_fungible_id())),
        }
    }

    pub fn liquid_amount(&self) -> Amount {
        self.container.liquid_amount()
    }

    pub fn resource_def_id(&self) -> ResourceDefId {
        self.container.resource_def_id()
    }

    pub fn resource_type(&self) -> ResourceType {
        self.container.resource_type()
    }

    pub fn borrow_container(&self) -> &ResourceContainer {
        &self.container
    }

    pub fn borrow_container_mut(&mut self) -> &mut ResourceContainer {
        &mut self.container
    }

    pub fn take_container(self) -> ResourceContainer {
        self.container
    }
}
