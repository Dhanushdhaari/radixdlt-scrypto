use sbor::*;
use scrypto::engine::types::*;
use scrypto::rust::collections::BTreeMap;
use scrypto::rust::collections::BTreeSet;
use scrypto::rust::collections::HashMap;
use scrypto::rust::rc::Rc;
use scrypto::rust::string::ToString;

/// Represents an error when manipulating resources in a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceContainerError {
    /// Resource addresses do not match
    ResourceAddressNotMatching,
    /// The amount is invalid, according to the resource divisibility
    InvalidAmount(Decimal, u8),
    /// The balance is not enough
    InsufficientBalance,
    /// Non-fungible operation on fungible resource is not allowed
    NonFungibleOperationNotAllowed,
}

#[derive(Debug, TypeId, Encode, Decode)]
pub enum ResourceContainer {
    // TODO: update state based on proofs.
    Fungible {
        /// The resource definition id
        resource_def_id: ResourceDefId,
        /// The resource divisibility
        divisibility: u8,
        /// The locked amounts and the corresponding times of being locked.
        locked_amounts: BTreeMap<Decimal, usize>,
        /// The liquid amount.
        liquid_amount: Decimal,
    },
    NonFungible {
        /// The resource definition id
        resource_def_id: ResourceDefId,
        /// The locked non-fungible ids and the corresponding times of being locked.
        locked_ids: HashMap<NonFungibleId, usize>,
        /// The liquid non-fungible ids.
        liquid_ids: BTreeSet<NonFungibleId>,
    },
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub enum ResourceContainerId {
    /// For named bucket
    Bucket(BucketId),
    /// For vault
    Vault(VaultId),
    /// For the specific resource on the n-th worktop
    Worktop {
        depth: u32,
        resource_def_id: ResourceDefId,
    },
}

#[derive(Debug, Clone)]
pub struct Proof {
    /// The resource definition id
    resource_def_id: ResourceDefId,
    /// The resource type
    resource_type: ResourceType,
    /// Restricted proof can't be moved down along the call stack (growing down).
    restricted: bool,
    /// The total amount for optimization purpose
    total_amount: Amount,
    /// The sub-amounts (to be extended)
    #[allow(dead_code)]
    amounts: HashMap<ResourceContainerId, (Rc<ResourceContainer>, Amount)>,
}

impl ResourceContainer {
    pub fn new_fungible(resource_def_id: ResourceDefId, divisibility: u8, amount: Decimal) -> Self {
        Self::Fungible {
            resource_def_id,
            divisibility,
            locked_amounts: BTreeMap::new(),
            liquid_amount: amount,
        }
    }

    pub fn new_non_fungible(resource_def_id: ResourceDefId, ids: BTreeSet<NonFungibleId>) -> Self {
        Self::NonFungible {
            resource_def_id,
            locked_ids: HashMap::new(),
            liquid_ids: ids.clone(),
        }
    }

    pub fn new_empty(resource_def_id: ResourceDefId, resource_type: ResourceType) -> Self {
        match resource_type {
            ResourceType::Fungible { divisibility } => {
                Self::new_fungible(resource_def_id, divisibility, Decimal::zero())
            }
            ResourceType::NonFungible => Self::new_non_fungible(resource_def_id, BTreeSet::new()),
        }
    }

    pub fn put(&mut self, other: Self) -> Result<(), ResourceContainerError> {
        // check resource address
        if self.resource_def_id() != other.resource_def_id() {
            return Err(ResourceContainerError::ResourceAddressNotMatching);
        }

        // assumption: owned bucket should not be locked
        assert!(!other.is_locked());

        // add the other bucket into liquid pool
        match (self, other.liquid_amount()) {
            (Self::Fungible { liquid_amount, .. }, Amount::Fungible { amount }) => {
                *liquid_amount = *liquid_amount + amount;
            }
            (Self::NonFungible { liquid_ids, .. }, Amount::NonFungible { ids }) => {
                liquid_ids.extend(ids);
            }
            _ => panic!("Resource type should match!"),
        }
        Ok(())
    }

    pub fn take(&mut self, quantity: Decimal) -> Result<Self, ResourceContainerError> {
        // check amount granularity
        let divisibility = self.resource_type().divisibility();
        Self::check_amount(quantity, divisibility)?;

        // deduct from liquidity pool
        match self {
            Self::Fungible { liquid_amount, .. } => {
                if *liquid_amount < quantity {
                    return Err(ResourceContainerError::InsufficientBalance);
                }
                *liquid_amount = *liquid_amount - quantity;
                Ok(Self::new_fungible(
                    self.resource_def_id(),
                    divisibility,
                    quantity,
                ))
            }
            Self::NonFungible { liquid_ids, .. } => {
                let n: usize = quantity.to_string().parse().unwrap();
                let taken: BTreeSet<NonFungibleId> = liquid_ids.iter().cloned().take(n).collect();
                taken.iter().for_each(|key| {
                    liquid_ids.remove(key);
                });
                Ok(Self::new_non_fungible(self.resource_def_id(), taken))
            }
        }
    }

    pub fn take_non_fungibles(
        &mut self,
        ids: &BTreeSet<NonFungibleId>,
    ) -> Result<Self, ResourceContainerError> {
        match self {
            Self::Fungible { .. } => Err(ResourceContainerError::NonFungibleOperationNotAllowed),
            Self::NonFungible { liquid_ids, .. } => {
                for id in ids {
                    if !liquid_ids.remove(&id) {
                        return Err(ResourceContainerError::InsufficientBalance);
                    }
                }
                Ok(Self::new_non_fungible(self.resource_def_id(), ids.clone()))
            }
        }
    }

    pub fn liquid_amount(&self) -> Amount {
        match self {
            Self::Fungible { liquid_amount, .. } => Amount::Fungible {
                amount: liquid_amount.clone(),
            },
            Self::NonFungible { liquid_ids, .. } => Amount::NonFungible {
                ids: liquid_ids.clone(),
            },
        }
    }

    pub fn is_locked(&self) -> bool {
        match self {
            Self::Fungible { locked_amounts, .. } => !locked_amounts.is_empty(),
            Self::NonFungible { locked_ids, .. } => !locked_ids.is_empty(),
        }
    }

    pub fn resource_def_id(&self) -> ResourceDefId {
        match self {
            Self::Fungible {
                resource_def_id, ..
            }
            | Self::NonFungible {
                resource_def_id, ..
            } => *resource_def_id,
        }
    }

    pub fn resource_type(&self) -> ResourceType {
        match self {
            Self::Fungible { divisibility, .. } => ResourceType::Fungible {
                divisibility: *divisibility,
            },
            Self::NonFungible { .. } => ResourceType::NonFungible,
        }
    }

    fn check_amount(amount: Decimal, divisibility: u8) -> Result<(), ResourceContainerError> {
        if !amount.is_negative() && amount.0 % 10i128.pow((18 - divisibility).into()) != 0.into() {
            Err(ResourceContainerError::InvalidAmount(amount, divisibility))
        } else {
            Ok(())
        }
    }
}

impl Proof {
    pub fn new(
        resource_container_id: ResourceContainerId,
        resource_container: Rc<ResourceContainer>,
    ) -> Self {
        let resource_def_id = resource_container.resource_def_id();
        let resource_type = resource_container.resource_type();
        let total_amount = resource_container.liquid_amount();
        let mut amounts = HashMap::new();
        amounts.insert(
            resource_container_id,
            (resource_container, total_amount.clone()),
        );
        Self {
            resource_def_id,
            resource_type,
            restricted: false,
            total_amount,
            amounts,
        }
    }

    pub fn resource_def_id(&self) -> ResourceDefId {
        self.resource_def_id
    }

    pub fn resource_type(&self) -> ResourceType {
        self.resource_type
    }

    pub fn total_amount(&self) -> Amount {
        self.total_amount.clone()
    }

    pub fn is_restricted(&self) -> bool {
        self.restricted
    }
}
