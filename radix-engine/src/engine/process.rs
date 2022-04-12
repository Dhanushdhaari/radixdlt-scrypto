use colored::*;

use sbor::*;
use scrypto::args;
use scrypto::buffer::*;
use scrypto::core::ScryptoActor;
use scrypto::engine::api::*;
use scrypto::engine::types::*;
use scrypto::rust::borrow::ToOwned;
use scrypto::rust::collections::*;
use scrypto::rust::fmt;
use scrypto::rust::format;
use scrypto::rust::string::String;
use scrypto::rust::string::ToString;
use scrypto::rust::vec;
use scrypto::rust::vec::Vec;
use scrypto::values::*;
use wasmi::*;

use crate::engine::process::LazyMapState::{Committed, Uncommitted};
use crate::engine::*;
use crate::errors::*;
use crate::ledger::*;
use crate::model::*;

macro_rules! re_trace {
    ($proc:expr, $($args: expr),+) => {
        if $proc.trace {
            $proc.log(Level::Trace, format!($($args),+));
        }
    };
}

macro_rules! re_debug {
    ($proc:expr, $($args: expr),+) => {
        if $proc.trace {
            $proc.log(Level::Debug, format!($($args),+));
        }
    };
}

macro_rules! re_info {
    ($proc:expr, $($args: expr),+) => {
        if $proc.trace {
            $proc.log(Level::Info, format!($($args),+));
        }
    };
}

macro_rules! re_warn {
    ($proc:expr, $($args: expr),+) => {
        if $proc.trace {
            $proc.log(Level::Warn, format!($($args),+));
        }
    };
}

pub enum SNodeState {
    Scrypto(
        ScryptoActorInfo,
        Option<(ComponentAddress, Vec<u8>, ScryptoValue)>,
    ),
    Resource(ResourceAddress),
    Bucket(Bucket),
    Vault(VaultId),
}

#[derive(Debug, Clone)]
pub enum SNodeRef {
    Scrypto(ScryptoActor),
    Resource(ResourceAddress),
    Bucket(BucketId),
    Vault(VaultId),
}

/// Represents an interpreter instance.
pub struct Interpreter {
    actor: ScryptoActorInfo,
    function: String,
    arg: ScryptoValue,
    module: ModuleRef,
    memory: MemoryRef,
}

/// Keeps invocation information.
#[derive(Debug, Clone)]
pub struct Invocation {
    snode_ref: SNodeRef,
    function: String,
    arg: ScryptoValue,
}

/// Qualitative states for a WASM process
#[derive(Debug)]
enum InterpreterState {
    Blueprint,
    Component {
        component_address: ComponentAddress,
        state: Vec<u8>,
        initial_loaded_object_refs: ComponentObjectRefs,
        additional_object_refs: ComponentObjectRefs,
    },
}

/// Top level state machine for a process. Empty currently only
/// refers to the initial process since it doesn't run on a wasm interpreter (yet)
#[allow(dead_code)]
struct WasmProcess {
    /// The call depth
    depth: usize,
    trace: bool,
    vm: Interpreter,
    interpreter_state: InterpreterState,
    process_owned_objects: ComponentObjects,
}

impl WasmProcess {
    fn check_resource(&self) -> bool {
        let mut success = true;

        for (vault_id, vault) in &self.process_owned_objects.vaults {
            re_warn!(self, "Dangling vault: {:?}, {:?}", vault_id, vault);
            success = false;
        }
        for (lazy_map_id, lazy_map) in &self.process_owned_objects.lazy_maps {
            re_warn!(self, "Dangling lazy map: {:?}, {:?}", lazy_map_id, lazy_map);
            success = false;
        }

        return success;
    }

    /// Logs a message to the console.
    #[allow(unused_variables)]
    pub fn log(&self, level: Level, msg: String) {
        let (l, m) = match level {
            Level::Error => ("ERROR".red(), msg.red()),
            Level::Warn => ("WARN".yellow(), msg.yellow()),
            Level::Info => ("INFO".green(), msg.green()),
            Level::Debug => ("DEBUG".cyan(), msg.cyan()),
            Level::Trace => ("TRACE".normal(), msg.normal()),
        };

        #[cfg(not(feature = "alloc"))]
        println!("{}[{:5}] {}", "  ".repeat(self.depth), l, m);
    }
}

///TODO: Remove
#[derive(Debug)]
enum LazyMapState {
    Uncommitted { root: LazyMapId },
    Committed { component_address: ComponentAddress },
}

impl<'s, S: SubstateStore> Track<'s, S> {
    fn insert_objects_into_component(
        &mut self,
        new_objects: ComponentObjects,
        component_address: ComponentAddress,
    ) {
        for (vault_id, vault) in new_objects.vaults {
            self.put_vault(component_address, vault_id, vault);
        }
        for (lazy_map_id, unclaimed) in new_objects.lazy_maps {
            for (k, v) in unclaimed.lazy_map {
                self.put_lazy_map_entry(component_address, lazy_map_id, k, v);
            }
            for (child_lazy_map_id, child_lazy_map) in unclaimed.descendent_lazy_maps {
                for (k, v) in child_lazy_map {
                    self.put_lazy_map_entry(component_address, child_lazy_map_id, k, v);
                }
            }
            for (vault_id, vault) in unclaimed.descendent_vaults {
                self.put_vault(component_address, vault_id, vault);
            }
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum MoveMethod {
    AsReturn,
    AsArgument,
}

/// A process keeps track of resource movements and code execution.
pub struct Process<'r, 'l, L: SubstateStore> {
    /// The call depth
    depth: usize,
    /// Whether to show trace messages
    trace: bool,
    /// Transactional state updates
    track: &'r mut Track<'l, L>,

    /// Buckets owned by this process
    buckets: HashMap<BucketId, Bucket>,
    /// Bucket proofs
    proofs: HashMap<ProofId, Proof>,

    /// State for the given wasm process, empty only on the root process
    /// (root process cannot create components nor is a component itself)
    wasm_process_state: Option<WasmProcess>,

    /// ID allocator for buckets and proofs created within transaction.
    id_allocator: IdAllocator,
    /// Resources collected from previous returns or self.
    worktop: Worktop,
    /// Proofs collected from previous returns or self. Also used for system authorization.
    auth_zone: Vec<Proof>,
    /// The caller's auth zone
    caller_auth_zone: &'r [Proof],
}

impl<'r, 'l, L: SubstateStore> Process<'r, 'l, L> {
    /// Create a new process, which is not started.
    pub fn new(depth: usize, trace: bool, track: &'r mut Track<'l, L>) -> Self {
        Self {
            depth,
            trace,
            track,
            buckets: HashMap::new(),
            proofs: HashMap::new(),
            wasm_process_state: None,
            id_allocator: IdAllocator::new(IdSpace::Transaction),
            worktop: Worktop::new(),
            auth_zone: Vec::new(),
            caller_auth_zone: &[],
        }
    }

    fn new_bucket_id(&mut self) -> Result<BucketId, RuntimeError> {
        if self.depth == 0 {
            self.id_allocator
                .new_bucket_id()
                .map_err(RuntimeError::IdAllocatorError)
        } else {
            Ok(self.track.new_bucket_id())
        }
    }

    fn new_proof_id(&mut self) -> Result<ProofId, RuntimeError> {
        if self.depth == 0 {
            self.id_allocator
                .new_proof_id()
                .map_err(RuntimeError::IdAllocatorError)
        } else {
            Ok(self.track.new_proof_id())
        }
    }

    // (Transaction ONLY) Takes resource by amount from worktop and returns a bucket.
    pub fn take_from_worktop(
        &mut self,
        amount: Decimal,
        resource_address: ResourceAddress,
    ) -> Result<BucketId, RuntimeError> {
        re_debug!(
            self,
            "(Transaction) Taking from worktop: {}, {}",
            amount,
            resource_address
        );
        let new_bucket_id = self.new_bucket_id()?;
        let new_bucket = match self
            .worktop
            .take(amount, resource_address)
            .map_err(RuntimeError::WorktopError)?
        {
            Some(bucket) => bucket,
            None => self.new_empty_bucket(resource_address)?,
        };
        self.buckets.insert(new_bucket_id, new_bucket);
        Ok(new_bucket_id)
    }

    // (Transaction ONLY) Takes resource by non-fungible IDs from worktop and returns a bucket.
    pub fn take_non_fungibles_from_worktop(
        &mut self,
        ids: &BTreeSet<NonFungibleId>,
        resource_address: ResourceAddress,
    ) -> Result<BucketId, RuntimeError> {
        re_debug!(
            self,
            "(Transaction) Taking from worktop: {:?}, {}",
            ids,
            resource_address
        );
        let new_bucket_id = self.new_bucket_id()?;
        let new_bucket = match self
            .worktop
            .take_non_fungibles(ids, resource_address)
            .map_err(RuntimeError::WorktopError)?
        {
            Some(bucket) => bucket,
            None => self.new_empty_bucket(resource_address)?,
        };
        self.buckets.insert(new_bucket_id, new_bucket);
        Ok(new_bucket_id)
    }

    // (Transaction ONLY) Takes resource by resource address from worktop and returns a bucket.
    pub fn take_all_from_worktop(
        &mut self,
        resource_address: ResourceAddress,
    ) -> Result<BucketId, RuntimeError> {
        re_debug!(
            self,
            "(Transaction) Taking from worktop: ALL, {}",
            resource_address
        );
        let new_bucket_id = self.new_bucket_id()?;
        let new_bucket = match self
            .worktop
            .take_all(resource_address)
            .map_err(RuntimeError::WorktopError)?
        {
            Some(bucket) => bucket,
            None => self.new_empty_bucket(resource_address)?,
        };
        self.buckets.insert(new_bucket_id, new_bucket);
        Ok(new_bucket_id)
    }

    fn new_empty_bucket(
        &mut self,
        resource_address: ResourceAddress,
    ) -> Result<Bucket, RuntimeError> {
        let resource_manager = self
            .track
            .get_resource_manager(&resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(resource_address))?;
        Ok(Bucket::new(ResourceContainer::new_empty(
            resource_address,
            resource_manager.resource_type(),
        )))
    }

    // (Transaction ONLY) Returns resource back to worktop.
    pub fn return_to_worktop(&mut self, bucket_id: BucketId) -> Result<ScryptoValue, RuntimeError> {
        re_debug!(
            self,
            "(Transaction) Returning to worktop: bucket_id = {}",
            bucket_id
        );

        let bucket = self
            .buckets
            .remove(&bucket_id)
            .ok_or(RuntimeError::BucketNotFound(bucket_id))?;
        self.worktop
            .put(bucket)
            .map_err(RuntimeError::WorktopError)?;
        Ok(ScryptoValue::from_value(&()))
    }

    // (Transaction ONLY) Assert worktop contains at least this amount.
    pub fn assert_worktop_contains(
        &mut self,
        resource_address: ResourceAddress,
    ) -> Result<ScryptoValue, RuntimeError> {
        if self.worktop.total_amount(resource_address).is_zero() {
            Err(RuntimeError::AssertionFailed)
        } else {
            Ok(ScryptoValue::from_value(&()))
        }
    }

    // (Transaction ONLY) Assert worktop contains at least this amount.
    pub fn assert_worktop_contains_by_amount(
        &mut self,
        amount: Decimal,
        resource_address: ResourceAddress,
    ) -> Result<ScryptoValue, RuntimeError> {
        if self.worktop.total_amount(resource_address) < amount {
            Err(RuntimeError::AssertionFailed)
        } else {
            Ok(ScryptoValue::from_value(&()))
        }
    }

    // (Transaction ONLY) Assert worktop contains at least this amount.
    pub fn assert_worktop_contains_by_ids(
        &mut self,
        ids: &BTreeSet<NonFungibleId>,
        resource_address: ResourceAddress,
    ) -> Result<ScryptoValue, RuntimeError> {
        if !self
            .worktop
            .total_ids(resource_address)
            .map_err(RuntimeError::WorktopError)?
            .is_superset(ids)
        {
            Err(RuntimeError::AssertionFailed)
        } else {
            Ok(ScryptoValue::from_value(&()))
        }
    }

    // Takes a proof from the auth zone.
    pub fn pop_from_auth_zone(&mut self) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Popping from auth zone");
        if self.auth_zone.is_empty() {
            return Err(RuntimeError::EmptyAuthZone);
        }

        let new_proof_id = self.new_proof_id()?;
        let proof = self.auth_zone.remove(self.auth_zone.len() - 1);
        self.proofs.insert(new_proof_id, proof);
        Ok(new_proof_id)
    }

    // Puts a proof onto the auth zone.
    pub fn push_to_auth_zone(&mut self, proof_id: ProofId) -> Result<(), RuntimeError> {
        re_debug!(self, "Pushing onto auth zone: proof_id = {}", proof_id);

        let proof = self
            .proofs
            .remove(&proof_id)
            .ok_or(RuntimeError::ProofNotFound(proof_id))?;

        if proof.is_restricted() {
            return Err(RuntimeError::CantMoveRestrictedProof(proof_id));
        }

        self.auth_zone.push(proof);
        Ok(())
    }

    // Creates a bucket proof.
    pub fn create_bucket_proof(&mut self, bucket_id: BucketId) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating proof: bucket_id = {}", bucket_id);

        let new_proof_id = self.new_proof_id()?;
        let bucket = self
            .buckets
            .get_mut(&bucket_id)
            .ok_or(RuntimeError::BucketNotFound(bucket_id))?;
        let new_proof = bucket
            .create_proof(ResourceContainerId::Bucket(bucket_id))
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    pub fn create_bucket_proof_by_amount(
        &mut self,
        bucket_id: BucketId,
        amount: Decimal,
    ) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating proof: bucket_id = {}", bucket_id);

        let new_proof_id = self.new_proof_id()?;
        let bucket = self
            .buckets
            .get_mut(&bucket_id)
            .ok_or(RuntimeError::BucketNotFound(bucket_id))?;
        let new_proof = bucket
            .create_proof_by_amount(amount, ResourceContainerId::Bucket(bucket_id))
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    pub fn create_bucket_proof_by_ids(
        &mut self,
        bucket_id: BucketId,
        ids: &BTreeSet<NonFungibleId>,
    ) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating bucket proof: bucket_id = {}", bucket_id);

        let new_proof_id = self.new_proof_id()?;
        let bucket = self
            .buckets
            .get_mut(&bucket_id)
            .ok_or(RuntimeError::BucketNotFound(bucket_id))?;
        let new_proof = bucket
            .create_proof_by_ids(ids, ResourceContainerId::Bucket(bucket_id))
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    // Creates a vault proof.
    pub fn create_vault_proof(&mut self, vault_id: VaultId) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating vault proof: vault_id = {:?}", vault_id);

        let new_proof_id = self.new_proof_id()?;
        let vault = self.get_local_vault(&vault_id)?;
        let new_proof = vault
            .create_proof(ResourceContainerId::Vault(vault_id))
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    pub fn create_vault_proof_by_amount(
        &mut self,
        vault_id: VaultId,
        amount: Decimal,
    ) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating vault proof: vault_id = {:?}", vault_id);

        let new_proof_id = self.new_proof_id()?;
        let vault = self.get_local_vault(&vault_id)?;
        let new_proof = vault
            .create_proof_by_amount(amount, ResourceContainerId::Vault(vault_id))
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    pub fn create_vault_proof_by_ids(
        &mut self,
        vault_id: VaultId,
        ids: &BTreeSet<NonFungibleId>,
    ) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating vault proof: vault_id = {:?}", vault_id);

        let new_proof_id = self.new_proof_id()?;
        let vault = self.get_local_vault(&vault_id)?;
        let new_proof = vault
            .create_proof_by_ids(ids, ResourceContainerId::Vault(vault_id))
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    // Creates a auth zone proof for all of the specified resource.
    pub fn create_auth_zone_proof(
        &mut self,
        resource_address: ResourceAddress,
    ) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating auth zone proof: ALL, {}", resource_address);

        let resource_manager = self
            .track
            .get_resource_manager(&resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(resource_address))?;
        let resource_type = resource_manager.resource_type();

        let new_proof_id = self.new_proof_id()?;
        let new_proof = Proof::compose(&self.auth_zone, resource_address, resource_type)
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    pub fn create_auth_zone_proof_by_amount(
        &mut self,
        amount: Decimal,
        resource_address: ResourceAddress,
    ) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating proof: {}, {}", amount, resource_address);

        let resource_manager = self
            .track
            .get_resource_manager(&resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(resource_address))?;
        let resource_type = resource_manager.resource_type();

        let new_proof_id = self.new_proof_id()?;
        let new_proof =
            Proof::compose_by_amount(&self.auth_zone, amount, resource_address, resource_type)
                .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    pub fn create_auth_zone_proof_by_ids(
        &mut self,
        ids: &BTreeSet<NonFungibleId>,
        resource_address: ResourceAddress,
    ) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Creating proof: {:?}, {}", ids, resource_address);

        let resource_manager = self
            .track
            .get_resource_manager(&resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(resource_address))?;
        let resource_type = resource_manager.resource_type();

        let new_proof_id = self.new_proof_id()?;
        let new_proof =
            Proof::compose_by_ids(&self.auth_zone, ids, resource_address, resource_type)
                .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    // Clone a proof.
    pub fn clone_proof(&mut self, proof_id: ProofId) -> Result<ProofId, RuntimeError> {
        re_debug!(self, "Cloning proof: proof_id = {}", proof_id);

        let new_proof_id = self.new_proof_id()?;
        let proof = self
            .proofs
            .get(&proof_id)
            .ok_or(RuntimeError::ProofNotFound(proof_id))?;
        let new_proof = proof.clone();
        self.proofs.insert(new_proof_id, new_proof);

        Ok(new_proof_id)
    }

    // Drop a proof.
    pub fn drop_proof(&mut self, proof_id: ProofId) -> Result<(), RuntimeError> {
        re_debug!(self, "Dropping proof: proof_id = {}", proof_id);

        let proof = self
            .proofs
            .remove(&proof_id)
            .ok_or(RuntimeError::ProofNotFound(proof_id))?;

        proof.drop();

        Ok(())
    }

    pub fn drop_all_named_proofs(&mut self) -> Result<(), RuntimeError> {
        let proof_ids: Vec<ProofId> = self.proofs.keys().cloned().collect();
        for proof_id in proof_ids {
            self.drop_proof(proof_id)?;
        }
        Ok(())
    }

    pub fn drop_all_auth_zone_proofs(&mut self) -> Result<(), RuntimeError> {
        loop {
            if let Some(proof) = self.auth_zone.pop() {
                proof.drop();
            } else {
                break;
            }
        }
        Ok(())
    }

    /// Drops all proofs owned by this process.
    pub fn drop_all_proofs(&mut self) -> Result<(), RuntimeError> {
        self.drop_all_named_proofs()?;
        self.drop_all_auth_zone_proofs()
    }

    /// (Transaction ONLY) Calls a method.
    pub fn call_method_with_all_resources(
        &mut self,
        component_address: ComponentAddress,
        method: &str,
    ) -> Result<ScryptoValue, RuntimeError> {
        re_debug!(
            self,
            "(Transaction) Calling method with all resources started"
        );

        // 1. Drop all proofs to unlock the buckets
        self.drop_all_proofs()?;

        // 2. Move collected resource to temp buckets
        for id in self.worktop.resource_addresses() {
            if let Some(bucket) = self
                .worktop
                .take_all(id)
                .map_err(RuntimeError::WorktopError)?
            {
                /*
                This is the only place that we don't follow the convention for bucket/proof ID generation.

                The reason is that the number of buckets to be created can't be determined statically, which
                makes it hard to verify transaction if we use the transaction ID allocator.
                */
                let bucket_id = self.track.new_bucket_id();
                self.buckets.insert(bucket_id, bucket);
            }
        }

        // 3. Call the method with all buckets
        let to_deposit: Vec<scrypto::resource::Bucket> = self
            .buckets
            .keys()
            .cloned()
            .map(|bucket_id| scrypto::resource::Bucket(bucket_id))
            .collect();

        // FIXME: This is horribly inefficient
        let arg = ScryptoValue::from_slice(&args!(to_deposit))
            .map_err(RuntimeError::ParseScryptoValueError)?;

        let invocation = Invocation {
            snode_ref: SNodeRef::Scrypto(ScryptoActor::Component(component_address)),
            function: method.to_owned(),
            arg,
        };
        let result = self.call(invocation);

        re_debug!(
            self,
            "(Transaction) Calling method with all resources ended"
        );
        result
    }

    pub fn publish_package(&mut self, code: Vec<u8>) -> Result<PackageAddress, RuntimeError> {
        re_debug!(self, "Publishing a package");

        let package = Package::new(code).map_err(RuntimeError::WasmValidationError)?;
        let package_address = self.track.create_package(package);
        Ok(package_address)
    }

    /// (SYSTEM ONLY)  Creates a proof which references a virtual bucket
    pub fn create_virtual_proof(
        &mut self,
        bucket_id: BucketId,
        proof_id: ProofId,
        mut bucket: Bucket,
    ) -> Result<(), RuntimeError> {
        let proof = bucket
            .create_proof(ResourceContainerId::Bucket(bucket_id))
            .map_err(RuntimeError::ProofError)?;
        self.proofs.insert(proof_id, proof);
        Ok(())
    }

    /// Runs the given export within this process.
    pub fn run(
        &mut self,
        snode: SNodeState,
        function: String,
        arg: ScryptoValue,
    ) -> Result<ScryptoValue, RuntimeError> {
        #[cfg(not(feature = "alloc"))]
        let now = std::time::Instant::now();
        re_info!(self, "Run started: function = {:?}", function);

        // Execution
        let output = match snode {
            SNodeState::Scrypto(actor, component_state) => {
                let package = self.track.get_package(actor.package_address()).ok_or(
                    RuntimeError::PackageNotFound(actor.package_address().clone()),
                )?;

                if !package.contains_blueprint(actor.blueprint_name()) {
                    return Err(RuntimeError::BlueprintNotFound(
                        actor.package_address().clone(),
                        actor.blueprint_name().to_string(),
                    ));
                }

                let (module, memory) = package.load_module().unwrap();

                let interpreter_state =
                    if let Some((component_address, state, data)) = component_state {
                        let initial_loaded_object_refs = ComponentObjectRefs {
                            vault_ids: data.vault_ids.into_iter().collect(),
                            lazy_map_ids: data.lazy_map_ids.into_iter().collect(),
                        };
                        InterpreterState::Component {
                            state,
                            component_address,
                            initial_loaded_object_refs,
                            additional_object_refs: ComponentObjectRefs::new(),
                        }
                    } else {
                        InterpreterState::Blueprint
                    };

                self.wasm_process_state = Some(WasmProcess {
                    depth: self.depth,
                    trace: self.trace,
                    vm: Interpreter {
                        function,
                        arg,
                        actor: actor.clone(),
                        module: module.clone(),
                        memory,
                    },
                    interpreter_state,
                    process_owned_objects: ComponentObjects::new(),
                });

                // Execution
                let result = module.invoke_export(actor.export_name(), &[], self);
                re_debug!(self, "Invoke result: {:?}", result);
                let rtn = result
                    .map_err(|e| {
                        match e.into_host_error() {
                            // Pass-through runtime errors
                            Some(host_error) => *host_error.downcast::<RuntimeError>().unwrap(),
                            None => RuntimeError::InvokeError,
                        }
                    })?
                    .ok_or(RuntimeError::NoReturnData)?;

                // Return value
                match rtn {
                    RuntimeValue::I32(ptr) => {
                        let data = self.read_return_value(ptr as u32)?;
                        self.process_return_data(&data)?;
                        Ok(data)
                    }
                    _ => {
                        return Err(RuntimeError::InvalidReturnType);
                    }
                }
            }
            SNodeState::Resource(resource_address) => {
                let maybe_bucket = self.track.resource_manager_invoke(
                    &resource_address,
                    function.as_str(),
                    arg,
                )?;
                if let Some(bucket) = maybe_bucket {
                    let bucket_id = self.new_bucket_id()?;
                    self.buckets.insert(bucket_id, bucket);
                    Ok(ScryptoValue::from_value(&scrypto::resource::Bucket(
                        bucket_id,
                    )))
                } else {
                    Ok(ScryptoValue::from_value(&()))
                }
            }

            _ => Err(RuntimeError::IllegalSystemCall),
        }?;

        #[cfg(not(feature = "alloc"))]
        re_info!(
            self,
            "Run ended: time elapsed = {} ms",
            now.elapsed().as_millis()
        );
        #[cfg(feature = "alloc")]
        re_info!(self, "Run ended");

        Ok(output)
    }

    /// Calls a function/method.
    pub fn call(&mut self, invocation: Invocation) -> Result<ScryptoValue, RuntimeError> {
        self.internal_call(invocation, false)
    }

    fn internal_call(
        &mut self,
        invocation: Invocation,
        force: bool,
    ) -> Result<ScryptoValue, RuntimeError> {
        // figure out what buckets and proofs to move from this process
        let mut moving_buckets = HashMap::new();
        let mut moving_proofs = HashMap::new();

        self.process_call_data(&invocation.arg)?;
        moving_buckets.extend(self.send_buckets(&invocation.arg.bucket_ids)?);
        moving_proofs.extend(self.send_proofs(&invocation.arg.proof_ids, MoveMethod::AsArgument)?);

        // Authorization and state load
        let (snode, method_auth) = match &invocation.snode_ref {
            SNodeRef::Scrypto(actor) => {
                match actor {
                    ScryptoActor::Blueprint(package_address, blueprint_name) => {
                        let export_name = format!("{}_main", blueprint_name);
                        Ok((
                            SNodeState::Scrypto(
                                ScryptoActorInfo::blueprint(
                                    package_address.clone(),
                                    blueprint_name.clone(),
                                    export_name.clone(),
                                ),
                                None,
                            ),
                            MethodAuthorization::Public,
                        ))
                    }
                    ScryptoActor::Component(component_address) => {
                        let component = self
                            .track
                            .get_component(component_address.clone())
                            .ok_or(RuntimeError::ComponentNotFound(component_address.clone()))?;
                        let package_address = component.package_address();
                        let blueprint_name = component.blueprint_name().to_string();
                        let component_state = component.state().to_vec();
                        let export_name = format!("{}_main", blueprint_name);

                        let package = self
                            .track
                            .get_package(&package_address)
                            .ok_or(RuntimeError::PackageNotFound(package_address))?;
                        // TODO: Remove clone
                        let schema = package
                            .load_blueprint_schema(&blueprint_name)
                            .unwrap()
                            .clone();

                        let component =
                            self.track.get_component(component_address.clone()).unwrap();
                        let (scrypto_value, method_auth) =
                            component.method_authorization(&schema, &invocation.function);
                        Ok((
                            SNodeState::Scrypto(
                                ScryptoActorInfo::component(
                                    package_address,
                                    blueprint_name,
                                    export_name,
                                    component_address.clone(),
                                ),
                                Some((component_address.clone(), component_state, scrypto_value)),
                            ),
                            method_auth,
                        ))
                    }
                }
            }
            SNodeRef::Resource(resource_address) => {
                let method_auth = self
                    .track
                    .get_resource_method_auth(&resource_address, &invocation.function)?;
                Ok((
                    SNodeState::Resource(resource_address.clone()),
                    method_auth.clone(),
                ))
            }
            SNodeRef::Bucket(bucket_id) => {
                let bucket = self
                    .buckets
                    .remove(&bucket_id)
                    .ok_or(RuntimeError::BucketNotFound(bucket_id.clone()))?;
                let resource_address = bucket.resource_address();
                let method_auth = self
                    .track
                    .get_resource_method_auth(&resource_address, &invocation.function)?;
                Ok((SNodeState::Bucket(bucket), method_auth.clone()))
            }
            SNodeRef::Vault(vault_id) => {
                let resource_address = self.get_local_vault(&vault_id)?.resource_address();
                let method_auth = self
                    .track
                    .get_resource_method_auth(&resource_address, &invocation.function)?;
                Ok((SNodeState::Vault(vault_id.clone()), method_auth.clone()))
            }
        }?;

        // Authorization check
        if !force {
            let proofs_vector = match &snode {
                // Same process auth check
                SNodeState::Vault(_) | SNodeState::Bucket(_) => {
                    vec![self.caller_auth_zone, &self.auth_zone]
                }
                // Extern call auth check
                _ => vec![self.auth_zone.as_slice()],
            };

            method_auth
                .check(&proofs_vector)
                .map_err(|e| RuntimeError::AuthorizationError(invocation.function.clone(), e))?;
        }

        // Execution
        let result = match snode {
            SNodeState::Vault(vault_id) => {
                let vault = self.get_local_vault(&vault_id)?;
                let maybe_bucket = vault
                    .main(invocation.function.as_str(), invocation.arg)
                    .map_err(RuntimeError::VaultError)?;
                if let Some(bucket) = maybe_bucket {
                    let bucket_id = self.new_bucket_id()?;
                    self.buckets.insert(bucket_id, bucket);
                    Ok(ScryptoValue::from_value(&scrypto::resource::Bucket(
                        bucket_id,
                    )))
                } else {
                    Ok(ScryptoValue::from_value(&()))
                }
            }
            SNodeState::Bucket(bucket) => match invocation.function.as_str() {
                "burn" => {
                    bucket.drop(self.track);
                    Ok(ScryptoValue::from_value(&()))
                }
                _ => Err(RuntimeError::IllegalSystemCall),
            },
            _ => {
                // start a new process
                let mut process = Process::new(self.depth + 1, self.trace, self.track);
                process.caller_auth_zone = &self.auth_zone;

                // move buckets and proofs to the new process.
                process.receive_buckets(moving_buckets)?;
                process.receive_proofs(moving_proofs)?;

                // invoke the main function
                let result = process.run(snode, invocation.function, invocation.arg)?;

                // figure out what buckets and resources to move from the new process
                let moving_buckets = process.send_buckets(&result.bucket_ids)?;
                let moving_proofs = process.send_proofs(&result.proof_ids, MoveMethod::AsReturn)?;

                // drop proofs and check resource leak
                process.drop_all_proofs()?;
                process.check_resource()?;

                // move buckets and proofs to this process.
                self.receive_buckets(moving_buckets)?;
                self.receive_proofs(moving_proofs)?;

                Ok(result)
            }
        }?;

        Ok(result)
    }

    /// Calls a function.
    pub fn call_function(
        &mut self,
        package_address: PackageAddress,
        blueprint_name: &str,
        function: &str,
        arg: ScryptoValue,
    ) -> Result<ScryptoValue, RuntimeError> {
        re_debug!(self, "Call function started");
        let invocation = Invocation {
            snode_ref: SNodeRef::Scrypto(ScryptoActor::Blueprint(
                package_address,
                blueprint_name.to_string(),
            )),
            function: function.to_string(),
            arg,
        };
        let result = self.call(invocation);
        re_debug!(self, "Call function ended");
        result
    }

    /// Calls a method.
    pub fn call_method(
        &mut self,
        component_address: ComponentAddress,
        method: &str,
        arg: ScryptoValue,
    ) -> Result<ScryptoValue, RuntimeError> {
        re_debug!(self, "Call method started");
        let invocation = Invocation {
            snode_ref: SNodeRef::Scrypto(ScryptoActor::Component(component_address)),
            function: method.to_owned(),
            arg,
        };
        let result = self.call(invocation);
        re_debug!(self, "Call method ended");
        result
    }

    /// Calls the ABI generator of a blueprint.
    // TODO: Remove
    pub fn call_abi(
        &mut self,
        package_address: PackageAddress,
        blueprint_name: &str,
    ) -> Result<ScryptoValue, RuntimeError> {
        re_debug!(self, "Call abi started");

        let snode = SNodeState::Scrypto(
            ScryptoActorInfo::blueprint(
                package_address,
                blueprint_name.to_string(),
                format!("{}_abi", blueprint_name),
            ),
            None,
        );

        let mut process = Process::new(self.depth + 1, self.trace, self.track);
        let result = process.run(snode, String::new(), ScryptoValue::from_value(&()));

        re_debug!(self, "Call abi ended");
        result
    }

    /// Checks resource leak.
    pub fn check_resource(&self) -> Result<(), RuntimeError> {
        re_debug!(self, "Resource check started");
        let mut success = true;

        for (bucket_id, bucket) in &self.buckets {
            re_warn!(self, "Dangling bucket: {}, {:?}", bucket_id, bucket);
            success = false;
        }
        if !self.worktop.is_empty() {
            re_warn!(self, "Resource worktop is not empty");
            success = false;
        }
        if let Some(wasm_process) = &self.wasm_process_state {
            if !wasm_process.check_resource() {
                success = false;
            }
        }

        re_debug!(self, "Resource check ended");
        if success {
            Ok(())
        } else {
            Err(RuntimeError::ResourceCheckFailure)
        }
    }

    /// Logs a message to the console.
    #[allow(unused_variables)]
    pub fn log(&self, level: Level, msg: String) {
        let (l, m) = match level {
            Level::Error => ("ERROR".red(), msg.red()),
            Level::Warn => ("WARN".yellow(), msg.yellow()),
            Level::Info => ("INFO".green(), msg.green()),
            Level::Debug => ("DEBUG".cyan(), msg.cyan()),
            Level::Trace => ("TRACE".normal(), msg.normal()),
        };

        #[cfg(not(feature = "alloc"))]
        println!("{}[{:5}] {}", "  ".repeat(self.depth), l, m);
    }

    fn process_call_data(&mut self, validated: &ScryptoValue) -> Result<(), RuntimeError> {
        if !validated.lazy_map_ids.is_empty() {
            return Err(RuntimeError::LazyMapNotAllowed);
        }
        if !validated.vault_ids.is_empty() {
            return Err(RuntimeError::VaultNotAllowed);
        }
        Ok(())
    }

    fn process_return_data(&mut self, validated: &ScryptoValue) -> Result<(), RuntimeError> {
        if !validated.lazy_map_ids.is_empty() {
            return Err(RuntimeError::LazyMapNotAllowed);
        }
        if !validated.vault_ids.is_empty() {
            return Err(RuntimeError::VaultNotAllowed);
        }
        Ok(())
    }

    /// Process and parse entry data from any component object (components and maps)
    fn process_entry_data(data: &[u8]) -> Result<ComponentObjectRefs, RuntimeError> {
        let validated =
            ScryptoValue::from_slice(data).map_err(RuntimeError::ParseScryptoValueError)?;
        if !validated.bucket_ids.is_empty() {
            return Err(RuntimeError::BucketNotAllowed);
        }
        if !validated.proof_ids.is_empty() {
            return Err(RuntimeError::ProofNotAllowed);
        }

        let mut lazy_map_ids = HashSet::new();
        for lazy_map_id in validated.lazy_map_ids {
            if lazy_map_ids.contains(&lazy_map_id) {
                return Err(RuntimeError::DuplicateLazyMap(lazy_map_id));
            }
            lazy_map_ids.insert(lazy_map_id);
        }

        let mut vault_ids = HashSet::new();
        for vault_id in validated.vault_ids {
            if vault_ids.contains(&vault_id) {
                return Err(RuntimeError::DuplicateVault(vault_id));
            }
            vault_ids.insert(vault_id);
        }

        // lazy map allowed
        // vaults allowed
        Ok(ComponentObjectRefs {
            lazy_map_ids,
            vault_ids,
        })
    }

    /// Sends buckets to another component/blueprint, either as argument or return
    fn send_buckets(
        &mut self,
        bucket_ids: &[BucketId],
    ) -> Result<HashMap<BucketId, Bucket>, RuntimeError> {
        let mut buckets = HashMap::new();
        for bucket_id in bucket_ids {
            let bucket = self
                .buckets
                .remove(bucket_id)
                .ok_or(RuntimeError::BucketNotFound(*bucket_id))?;
            re_debug!(self, "Moving bucket: {}, {:?}", bucket_id, bucket);
            if bucket.is_locked() {
                return Err(RuntimeError::CantMoveLockedBucket);
            }
            buckets.insert(*bucket_id, bucket);
        }
        Ok(buckets)
    }

    /// Receives buckets from another component/blueprint, either as argument or return
    fn receive_buckets(&mut self, buckets: HashMap<BucketId, Bucket>) -> Result<(), RuntimeError> {
        if self.depth == 0 {
            // buckets are aggregated by worktop
            for (_, bucket) in buckets {
                self.worktop
                    .put(bucket)
                    .map_err(RuntimeError::WorktopError)?;
            }
        } else {
            // for component, received buckets go to the "buckets" areas.
            self.buckets.extend(buckets);
        }

        Ok(())
    }

    /// Sends proofs to another component/blueprint, either as argument or return
    fn send_proofs(
        &mut self,
        proof_ids: &[ProofId],
        method: MoveMethod,
    ) -> Result<HashMap<ProofId, Proof>, RuntimeError> {
        let mut proofs = HashMap::new();
        for proof_id in proof_ids {
            let mut proof = self
                .proofs
                .remove(proof_id)
                .ok_or(RuntimeError::ProofNotFound(*proof_id))?;
            re_debug!(self, "Moving proof: {}, {:?}", proof_id, proof);
            if proof.is_restricted() {
                return Err(RuntimeError::CantMoveRestrictedProof(*proof_id));
            }
            if matches!(method, MoveMethod::AsArgument) {
                proof.change_to_restricted();
            }
            proofs.insert(*proof_id, proof);
        }
        Ok(proofs)
    }

    /// Receives proofs from another component/blueprint, either as argument or return
    fn receive_proofs(&mut self, proofs: HashMap<ProofId, Proof>) -> Result<(), RuntimeError> {
        if self.depth == 0 {
            // proofs are accumulated by auth worktop
            for (_, proof) in proofs {
                self.auth_zone.push(proof);
            }
        } else {
            // for component, received buckets go to the "proofs" areas.
            for (proof_id, proof) in proofs {
                self.proofs.insert(proof_id, proof);
            }
        }

        Ok(())
    }

    /// Send a byte array to wasm instance.
    fn send_bytes(&mut self, bytes: &[u8]) -> Result<i32, RuntimeError> {
        let wasm_process = self.wasm_process_state.as_ref().unwrap();
        let result = wasm_process.vm.module.invoke_export(
            "scrypto_alloc",
            &[RuntimeValue::I32((bytes.len()) as i32)],
            &mut NopExternals,
        );

        if let Ok(Some(RuntimeValue::I32(ptr))) = result {
            if wasm_process.vm.memory.set((ptr + 4) as u32, bytes).is_ok() {
                return Ok(ptr);
            }
        }

        Err(RuntimeError::MemoryAllocError)
    }

    fn read_return_value(&mut self, ptr: u32) -> Result<ScryptoValue, RuntimeError> {
        let wasm_process = self.wasm_process_state.as_ref().unwrap();
        // read length
        let len: u32 = wasm_process
            .vm
            .memory
            .get_value(ptr)
            .map_err(|_| RuntimeError::MemoryAccessError)?;

        let start = ptr.checked_add(4).ok_or(RuntimeError::MemoryAccessError)?;
        let end = start
            .checked_add(len)
            .ok_or(RuntimeError::MemoryAccessError)?;
        let range = start as usize..end as usize;
        let direct = wasm_process.vm.memory.direct_access();
        let buffer = direct.as_ref();

        if end > buffer.len().try_into().unwrap() {
            return Err(RuntimeError::MemoryAccessError);
        }

        ScryptoValue::from_slice(&buffer[range]).map_err(RuntimeError::ParseScryptoValueError)
    }

    /// Handles a system call.
    fn handle<I: Decode + fmt::Debug, O: Encode + fmt::Debug>(
        &mut self,
        args: RuntimeArgs,
        handler: fn(&mut Self, input: I) -> Result<O, RuntimeError>,
    ) -> Result<Option<RuntimeValue>, Trap> {
        let wasm_process = self.wasm_process_state.as_mut().unwrap();
        let op: u32 = args.nth_checked(0)?;
        let input_ptr: u32 = args.nth_checked(1)?;
        let input_len: u32 = args.nth_checked(2)?;
        // SECURITY: bill before allocating memory
        let mut input_bytes = vec![0u8; input_len as usize];
        wasm_process
            .vm
            .memory
            .get_into(input_ptr, &mut input_bytes)
            .map_err(|_| Trap::from(RuntimeError::MemoryAccessError))?;
        let input: I = scrypto_decode(&input_bytes)
            .map_err(|e| Trap::from(RuntimeError::InvalidRequestData(e)))?;
        if input_len <= 1024 {
            re_trace!(self, "{:?}", input);
        } else {
            re_trace!(self, "Large request: op = {:02x}, len = {}", op, input_len);
        }

        let output: O = handler(self, input).map_err(Trap::from)?;
        let output_bytes = scrypto_encode(&output);
        let output_ptr = self.send_bytes(&output_bytes).map_err(Trap::from)?;
        if output_bytes.len() <= 1024 {
            re_trace!(self, "{:?}", output);
        } else {
            re_trace!(
                self,
                "Large response: op = {:02x}, len = {}",
                op,
                output_bytes.len()
            );
        }

        Ok(Some(RuntimeValue::I32(output_ptr)))
    }

    //============================
    // SYSTEM CALL HANDLERS START
    //============================

    fn handle_publish(
        &mut self,
        input: PublishPackageInput,
    ) -> Result<PublishPackageOutput, RuntimeError> {
        let package_address = self.publish_package(input.code)?;
        Ok(PublishPackageOutput { package_address })
    }

    fn handle_call_function(
        &mut self,
        input: CallFunctionInput,
    ) -> Result<CallFunctionOutput, RuntimeError> {
        let arg =
            ScryptoValue::from_slice(&input.arg).map_err(RuntimeError::ParseScryptoValueError)?;

        re_debug!(
            self,
            "CALL started: package_address = {}, blueprint_name = {}, function = {}, arg = {:?}",
            input.package_address,
            input.blueprint_name,
            input.function,
            arg
        );
        let result = self.call_function(
            input.package_address,
            &input.blueprint_name,
            &input.function,
            arg,
        );
        re_debug!(self, "CALL finished");
        Ok(CallFunctionOutput { rtn: result?.raw })
    }

    fn handle_call_method(
        &mut self,
        input: CallMethodInput,
    ) -> Result<CallMethodOutput, RuntimeError> {
        let arg =
            ScryptoValue::from_slice(&input.arg).map_err(RuntimeError::ParseScryptoValueError)?;

        re_debug!(
            self,
            "CALL started: component = {}, method = {}, arg = {:?}",
            input.component_address,
            input.method,
            arg
        );

        let result = self.call_method(input.component_address, input.method.as_str(), arg);
        re_debug!(self, "CALL finished");
        Ok(CallMethodOutput { rtn: result?.raw })
    }

    fn handle_create_component(
        &mut self,
        input: CreateComponentInput,
    ) -> Result<CreateComponentOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;

        let data = Self::process_entry_data(&input.state)?;
        let new_objects = wasm_process.process_owned_objects.take(data)?;
        let package_address = wasm_process.vm.actor.package_address().clone();
        let component = Component::new(
            package_address,
            input.blueprint_name,
            input.authorization,
            input.state,
        );
        let component_address = self.track.create_component(component);
        self.track
            .insert_objects_into_component(new_objects, component_address);

        Ok(CreateComponentOutput { component_address })
    }

    fn handle_get_component_info(
        &mut self,
        input: GetComponentInfoInput,
    ) -> Result<GetComponentInfoOutput, RuntimeError> {
        let component = self
            .track
            .get_component(input.component_address)
            .ok_or(RuntimeError::ComponentNotFound(input.component_address))?;

        Ok(GetComponentInfoOutput {
            package_address: component.package_address(),
            blueprint_name: component.blueprint_name().to_owned(),
        })
    }

    fn handle_get_component_state(
        &mut self,
        _: GetComponentStateInput,
    ) -> Result<GetComponentStateOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;
        let return_state = match &wasm_process.interpreter_state {
            InterpreterState::Component { state, .. } => Ok(state),
            _ => Err(RuntimeError::IllegalSystemCall),
        }?;
        let state = return_state.to_vec();
        Ok(GetComponentStateOutput { state })
    }

    fn handle_put_component_state(
        &mut self,
        input: PutComponentStateInput,
    ) -> Result<PutComponentStateOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;
        match &wasm_process.interpreter_state {
            InterpreterState::Component {
                component_address,
                initial_loaded_object_refs,
                ..
            } => {
                let mut new_set = Self::process_entry_data(&input.state)?;
                new_set.remove(&initial_loaded_object_refs)?;
                let new_objects = wasm_process.process_owned_objects.take(new_set)?;
                self.track
                    .insert_objects_into_component(new_objects, *component_address);

                // TODO: Verify that process_owned_objects is empty

                let component = self.track.get_component_mut(*component_address).unwrap();
                component.set_state(input.state);
                Ok(())
            }
            _ => Err(RuntimeError::IllegalSystemCall),
        }?;

        Ok(PutComponentStateOutput {})
    }

    fn handle_create_lazy_map(
        &mut self,
        _input: CreateLazyMapInput,
    ) -> Result<CreateLazyMapOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;
        let lazy_map_id = self.track.new_lazy_map_id();
        wasm_process
            .process_owned_objects
            .lazy_maps
            .insert(lazy_map_id, UnclaimedLazyMap::new());
        Ok(CreateLazyMapOutput { lazy_map_id })
    }

    fn handle_get_lazy_map_entry(
        &mut self,
        input: GetLazyMapEntryInput,
    ) -> Result<GetLazyMapEntryOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;
        let entry = match wasm_process
            .process_owned_objects
            .get_lazy_map_entry(&input.lazy_map_id, &input.key)
        {
            None => match &mut wasm_process.interpreter_state {
                InterpreterState::Component {
                    initial_loaded_object_refs,
                    additional_object_refs,
                    component_address,
                    ..
                } => {
                    if !initial_loaded_object_refs
                        .lazy_map_ids
                        .contains(&input.lazy_map_id)
                        && !additional_object_refs
                            .lazy_map_ids
                            .contains(&input.lazy_map_id)
                    {
                        return Err(RuntimeError::LazyMapNotFound(input.lazy_map_id));
                    }
                    let value = self.track.get_lazy_map_entry(
                        *component_address,
                        &input.lazy_map_id,
                        &input.key,
                    );
                    if value.is_some() {
                        let map_entry_objects =
                            Self::process_entry_data(&value.as_ref().unwrap()).unwrap();
                        additional_object_refs.extend(map_entry_objects);
                    }

                    Ok(value)
                }
                _ => Err(RuntimeError::LazyMapNotFound(input.lazy_map_id)),
            },
            Some((_, value)) => Ok(value),
        }?;

        Ok(GetLazyMapEntryOutput { value: entry })
    }

    fn handle_put_lazy_map_entry(
        &mut self,
        input: PutLazyMapEntryInput,
    ) -> Result<PutLazyMapEntryOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;
        let (old_value, lazy_map_state) = match wasm_process
            .process_owned_objects
            .get_lazy_map_entry(&input.lazy_map_id, &input.key)
        {
            None => match &wasm_process.interpreter_state {
                InterpreterState::Component {
                    initial_loaded_object_refs,
                    additional_object_refs,
                    component_address,
                    ..
                } => {
                    if !initial_loaded_object_refs
                        .lazy_map_ids
                        .contains(&input.lazy_map_id)
                        && !additional_object_refs
                            .lazy_map_ids
                            .contains(&input.lazy_map_id)
                    {
                        return Err(RuntimeError::LazyMapNotFound(input.lazy_map_id));
                    }
                    let old_value = self.track.get_lazy_map_entry(
                        *component_address,
                        &input.lazy_map_id,
                        &input.key,
                    );
                    Ok((
                        old_value,
                        Committed {
                            component_address: *component_address,
                        },
                    ))
                }
                _ => Err(RuntimeError::LazyMapNotFound(input.lazy_map_id)),
            },
            Some((root, value)) => Ok((value, Uncommitted { root })),
        }?;
        let mut new_entry_object_refs = Self::process_entry_data(&input.value)?;
        let old_entry_object_refs = match old_value {
            None => ComponentObjectRefs::new(),
            Some(e) => Self::process_entry_data(&e).unwrap(),
        };
        new_entry_object_refs.remove(&old_entry_object_refs)?;

        // Check for cycles
        if let Uncommitted { root } = lazy_map_state {
            if new_entry_object_refs.lazy_map_ids.contains(&root) {
                return Err(RuntimeError::CyclicLazyMap(root));
            }
        }

        let new_objects = wasm_process
            .process_owned_objects
            .take(new_entry_object_refs)?;

        match lazy_map_state {
            Uncommitted { root } => {
                wasm_process.process_owned_objects.insert_lazy_map_entry(
                    &input.lazy_map_id,
                    input.key,
                    input.value,
                );
                wasm_process
                    .process_owned_objects
                    .insert_objects_into_map(new_objects, &root);
            }
            Committed { component_address } => {
                self.track.put_lazy_map_entry(
                    component_address,
                    input.lazy_map_id,
                    input.key,
                    input.value,
                );
                self.track
                    .insert_objects_into_component(new_objects, component_address);
            }
        }

        Ok(PutLazyMapEntryOutput {})
    }

    fn handle_create_resource(
        &mut self,
        input: CreateResourceInput,
    ) -> Result<CreateResourceOutput, RuntimeError> {
        let resource_manager =
            ResourceManager::new(input.resource_type, input.metadata, input.authorization)
                .map_err(RuntimeError::ResourceManagerError)?;

        let resource_address = self.track.create_resource_manager(resource_manager);
        re_debug!(self, "New resource manager: {}", resource_address);

        let bucket_id = if let Some(mint_params) = input.mint_params {
            let invocation = Invocation {
                snode_ref: SNodeRef::Resource(resource_address.clone()),
                function: "mint".to_string(),
                arg: ScryptoValue::from_value(&mint_params),
            };
            // TODO: Remove force
            let result = self.internal_call(invocation, true)?;
            Some(result.bucket_ids[0])
        } else {
            None
        };

        Ok(CreateResourceOutput {
            resource_address,
            bucket_id,
        })
    }

    fn handle_get_resource_metadata(
        &mut self,
        input: GetResourceMetadataInput,
    ) -> Result<GetResourceMetadataOutput, RuntimeError> {
        let resource_manager = self
            .track
            .get_resource_manager(&input.resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(
                input.resource_address,
            ))?;

        Ok(GetResourceMetadataOutput {
            metadata: resource_manager.metadata().clone(),
        })
    }

    fn handle_get_resource_total_supply(
        &mut self,
        input: GetResourceTotalSupplyInput,
    ) -> Result<GetResourceTotalSupplyOutput, RuntimeError> {
        let resource_manager = self
            .track
            .get_resource_manager(&input.resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(
                input.resource_address,
            ))?;

        Ok(GetResourceTotalSupplyOutput {
            total_supply: resource_manager.total_supply(),
        })
    }

    fn handle_get_resource_type(
        &mut self,
        input: GetResourceTypeInput,
    ) -> Result<GetResourceTypeOutput, RuntimeError> {
        let resource_manager = self
            .track
            .get_resource_manager(&input.resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(
                input.resource_address,
            ))?;

        Ok(GetResourceTypeOutput {
            resource_type: resource_manager.resource_type(),
        })
    }

    fn handle_get_non_fungible_data(
        &mut self,
        input: GetNonFungibleDataInput,
    ) -> Result<GetNonFungibleDataOutput, RuntimeError> {
        let non_fungible = self
            .track
            .get_non_fungible(&input.non_fungible_address)
            .ok_or(RuntimeError::NonFungibleNotFound(
                input.non_fungible_address,
            ))?;

        Ok(GetNonFungibleDataOutput {
            immutable_data: non_fungible.immutable_data(),
            mutable_data: non_fungible.mutable_data(),
        })
    }

    fn handle_non_fungible_exists(
        &mut self,
        input: NonFungibleExistsInput,
    ) -> Result<NonFungibleExistsOutput, RuntimeError> {
        let non_fungible = self.track.get_non_fungible(&input.non_fungible_address);

        Ok(NonFungibleExistsOutput {
            non_fungible_exists: non_fungible.is_some(),
        })
    }

    fn handle_create_vault(
        &mut self,
        input: CreateEmptyVaultInput,
    ) -> Result<CreateEmptyVaultOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;
        let definition = self
            .track
            .get_resource_manager(&input.resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(
                input.resource_address,
            ))?;

        let new_vault = Vault::new(ResourceContainer::new_empty(
            input.resource_address,
            definition.resource_type(),
        ));
        let vault_id = self.track.new_vault_id();
        wasm_process
            .process_owned_objects
            .vaults
            .insert(vault_id, new_vault);

        Ok(CreateEmptyVaultOutput { vault_id })
    }

    fn get_local_vault(&mut self, vault_id: &VaultId) -> Result<&mut Vault, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_mut()
            .ok_or(RuntimeError::IllegalSystemCall)?;
        match wasm_process.process_owned_objects.get_vault_mut(vault_id) {
            Some(vault) => Ok(vault),
            None => match &wasm_process.interpreter_state {
                InterpreterState::Component {
                    component_address,
                    initial_loaded_object_refs,
                    additional_object_refs,
                    ..
                } => {
                    if !initial_loaded_object_refs.vault_ids.contains(vault_id)
                        && !additional_object_refs.vault_ids.contains(vault_id)
                    {
                        return Err(RuntimeError::VaultNotFound(*vault_id));
                    }
                    let vault = self.track.get_vault_mut(component_address, vault_id);
                    Ok(vault)
                }
                _ => Err(RuntimeError::VaultNotFound(*vault_id)),
            },
        }
    }

    fn handle_put_into_vault(
        &mut self,
        input: PutIntoVaultInput,
    ) -> Result<PutIntoVaultOutput, RuntimeError> {
        // TODO: restrict access

        let bucket = self
            .buckets
            .remove(&input.bucket_id)
            .ok_or(RuntimeError::BucketNotFound(input.bucket_id))?;

        self.get_local_vault(&input.vault_id)?
            .put(bucket)
            .map_err(|e| RuntimeError::VaultError(VaultError::ResourceContainerError(e)))?;

        Ok(PutIntoVaultOutput {})
    }

    fn handle_update_resource_metadata(
        &mut self,
        input: UpdateResourceMetadataInput,
    ) -> Result<UpdateResourceMetadataOutput, RuntimeError> {
        let invocation = Invocation {
            snode_ref: SNodeRef::Resource(input.resource_address.clone()),
            function: "update_metadata".to_string(),
            arg: ScryptoValue::from_value(&input.new_metadata),
        };
        let _ = self.call(invocation)?;
        Ok(UpdateResourceMetadataOutput {})
    }

    fn handle_update_non_fungible_mutable_data(
        &mut self,
        input: UpdateNonFungibleMutableDataInput,
    ) -> Result<UpdateNonFungibleMutableDataOutput, RuntimeError> {
        let invocation = Invocation {
            snode_ref: SNodeRef::Resource(input.non_fungible_address.resource_address()),
            function: "update_non_fungible_mutable_data".to_string(),
            arg: ScryptoValue::from_value(&(
                input.non_fungible_address.non_fungible_id(),
                input.new_mutable_data,
            )),
        };
        let _ = self.call(invocation)?;
        Ok(UpdateNonFungibleMutableDataOutput {})
    }

    fn handle_mint_resource(
        &mut self,
        input: MintResourceInput,
    ) -> Result<MintResourceOutput, RuntimeError> {
        let invocation = Invocation {
            snode_ref: SNodeRef::Resource(input.resource_address.clone()),
            function: "mint".to_string(),
            arg: ScryptoValue::from_value(&input.mint_params),
        };
        let result = self.call(invocation)?;
        Ok(MintResourceOutput {
            bucket_id: result.bucket_ids[0],
        })
    }

    fn handle_burn_resource(
        &mut self,
        input: BurnResourceInput,
    ) -> Result<BurnResourceOutput, RuntimeError> {
        let invocation = Invocation {
            snode_ref: SNodeRef::Bucket(input.bucket_id.clone()),
            function: "burn".to_string(),
            arg: ScryptoValue::from_value(&()),
        };
        let _ = self.call(invocation)?;
        Ok(BurnResourceOutput {})
    }

    fn handle_take_from_vault(
        &mut self,
        input: TakeFromVaultInput,
    ) -> Result<TakeFromVaultOutput, RuntimeError> {
        let invocation = Invocation {
            snode_ref: SNodeRef::Vault(input.vault_id.clone()),
            function: "take_from_vault".to_string(),
            arg: ScryptoValue::from_value(&input.amount),
        };
        let result = self.call(invocation)?;
        Ok(TakeFromVaultOutput {
            bucket_id: result.bucket_ids[0],
        })
    }

    fn handle_take_non_fungibles_from_vault(
        &mut self,
        input: TakeNonFungiblesFromVaultInput,
    ) -> Result<TakeNonFungiblesFromVaultOutput, RuntimeError> {
        let invocation = Invocation {
            snode_ref: SNodeRef::Vault(input.vault_id.clone()),
            function: "take_non_fungibles_from_vault".to_string(),
            arg: ScryptoValue::from_value(&input.non_fungible_ids),
        };
        let result = self.call(invocation)?;
        Ok(TakeNonFungiblesFromVaultOutput {
            bucket_id: result.bucket_ids[0],
        })
    }

    fn handle_get_non_fungible_ids_in_vault(
        &mut self,
        input: GetNonFungibleIdsInVaultInput,
    ) -> Result<GetNonFungibleIdsInVaultOutput, RuntimeError> {
        let vault = self.get_local_vault(&input.vault_id)?;
        let non_fungible_ids = vault
            .total_ids()
            .map_err(|e| RuntimeError::VaultError(VaultError::ResourceContainerError(e)))?
            .into_iter()
            .collect();

        Ok(GetNonFungibleIdsInVaultOutput { non_fungible_ids })
    }

    fn handle_get_vault_amount(
        &mut self,
        input: GetVaultAmountInput,
    ) -> Result<GetVaultAmountOutput, RuntimeError> {
        let vault = self.get_local_vault(&input.vault_id)?;

        Ok(GetVaultAmountOutput {
            amount: vault.total_amount(),
        })
    }

    fn handle_get_vault_resource_address(
        &mut self,
        input: GetVaultResourceAddressInput,
    ) -> Result<GetVaultResourceAddressOutput, RuntimeError> {
        let vault = self.get_local_vault(&input.vault_id)?;

        Ok(GetVaultResourceAddressOutput {
            resource_address: vault.resource_address(),
        })
    }

    fn handle_create_bucket(
        &mut self,
        input: CreateEmptyBucketInput,
    ) -> Result<CreateEmptyBucketOutput, RuntimeError> {
        let definition = self
            .track
            .get_resource_manager(&input.resource_address)
            .ok_or(RuntimeError::ResourceManagerNotFound(
                input.resource_address,
            ))?;

        let new_bucket = Bucket::new(ResourceContainer::new_empty(
            input.resource_address,
            definition.resource_type(),
        ));
        let bucket_id = self.new_bucket_id()?;
        self.buckets.insert(bucket_id, new_bucket);

        Ok(CreateEmptyBucketOutput { bucket_id })
    }

    fn handle_put_into_bucket(
        &mut self,
        input: PutIntoBucketInput,
    ) -> Result<PutIntoBucketOutput, RuntimeError> {
        let other = self
            .buckets
            .remove(&input.other)
            .ok_or(RuntimeError::BucketNotFound(input.other))?;

        self.buckets
            .get_mut(&input.bucket_id)
            .ok_or(RuntimeError::BucketNotFound(input.bucket_id))?
            .put(other)
            .map_err(RuntimeError::BucketError)?;

        Ok(PutIntoBucketOutput {})
    }

    fn handle_take_from_bucket(
        &mut self,
        input: TakeFromBucketInput,
    ) -> Result<TakeFromBucketOutput, RuntimeError> {
        let new_bucket = self
            .buckets
            .get_mut(&input.bucket_id)
            .ok_or(RuntimeError::BucketNotFound(input.bucket_id))?
            .take(input.amount)
            .map_err(RuntimeError::BucketError)?;
        let bucket_id = self.new_bucket_id()?;
        self.buckets.insert(bucket_id, new_bucket);

        Ok(TakeFromBucketOutput { bucket_id })
    }

    fn handle_get_bucket_amount(
        &mut self,
        input: GetBucketAmountInput,
    ) -> Result<GetBucketAmountOutput, RuntimeError> {
        let amount = self
            .buckets
            .get(&input.bucket_id)
            .map(|b| b.total_amount())
            .ok_or(RuntimeError::BucketNotFound(input.bucket_id))?;

        Ok(GetBucketAmountOutput { amount })
    }

    fn handle_get_bucket_resource_address(
        &mut self,
        input: GetBucketResourceAddressInput,
    ) -> Result<GetBucketResourceAddressOutput, RuntimeError> {
        let resource_address = self
            .buckets
            .get(&input.bucket_id)
            .map(|b| b.resource_address())
            .ok_or(RuntimeError::BucketNotFound(input.bucket_id))?;

        Ok(GetBucketResourceAddressOutput { resource_address })
    }

    fn handle_take_non_fungibles_from_bucket(
        &mut self,
        input: TakeNonFungiblesFromBucketInput,
    ) -> Result<TakeNonFungiblesFromBucketOutput, RuntimeError> {
        let new_bucket = self
            .buckets
            .get_mut(&input.bucket_id)
            .ok_or(RuntimeError::BucketNotFound(input.bucket_id))?
            .take_non_fungibles(&input.non_fungible_ids)
            .map_err(RuntimeError::BucketError)?;
        let bucket_id = self.new_bucket_id()?;
        self.buckets.insert(bucket_id, new_bucket);

        Ok(TakeNonFungiblesFromBucketOutput { bucket_id })
    }

    fn handle_get_non_fungible_ids_in_bucket(
        &mut self,
        input: GetNonFungibleIdsInBucketInput,
    ) -> Result<GetNonFungibleIdsInBucketOutput, RuntimeError> {
        let bucket = self
            .buckets
            .get(&input.bucket_id)
            .ok_or(RuntimeError::BucketNotFound(input.bucket_id))?;

        Ok(GetNonFungibleIdsInBucketOutput {
            non_fungible_ids: bucket
                .total_ids()
                .map_err(RuntimeError::BucketError)?
                .into_iter()
                .collect(),
        })
    }

    fn handle_create_bucket_proof(
        &mut self,
        input: CreateBucketProofInput,
    ) -> Result<CreateBucketProofOutput, RuntimeError> {
        Ok(CreateBucketProofOutput {
            proof_id: self.create_bucket_proof(input.bucket_id)?,
        })
    }

    fn handle_create_vault_proof(
        &mut self,
        input: CreateVaultProofInput,
    ) -> Result<CreateVaultProofOutput, RuntimeError> {
        Ok(CreateVaultProofOutput {
            proof_id: self.create_vault_proof(input.vault_id)?,
        })
    }

    fn handle_create_vault_proof_by_amount(
        &mut self,
        input: CreateVaultProofByAmountInput,
    ) -> Result<CreateVaultProofByAmountOutput, RuntimeError> {
        Ok(CreateVaultProofByAmountOutput {
            proof_id: self.create_vault_proof_by_amount(input.vault_id, input.amount)?,
        })
    }

    fn handle_create_vault_proof_by_ids(
        &mut self,
        input: CreateVaultProofByIdsInput,
    ) -> Result<CreateVaultProofByIdsOutput, RuntimeError> {
        Ok(CreateVaultProofByIdsOutput {
            proof_id: self.create_vault_proof_by_ids(input.vault_id, &input.ids)?,
        })
    }

    fn handle_create_auth_zone_proof(
        &mut self,
        input: CreateAuthZoneProofInput,
    ) -> Result<CreateAuthZoneProofOutput, RuntimeError> {
        Ok(CreateAuthZoneProofOutput {
            proof_id: self.create_auth_zone_proof(input.resource_address)?,
        })
    }

    fn handle_create_auth_zone_proof_by_amount(
        &mut self,
        input: CreateAuthZoneProofByAmountInput,
    ) -> Result<CreateAuthZoneProofByAmountOutput, RuntimeError> {
        Ok(CreateAuthZoneProofByAmountOutput {
            proof_id: self
                .create_auth_zone_proof_by_amount(input.amount, input.resource_address)?,
        })
    }

    fn handle_create_auth_zone_proof_by_ids(
        &mut self,
        input: CreateAuthZoneProofByIdsInput,
    ) -> Result<CreateAuthZoneProofByIdsOutput, RuntimeError> {
        Ok(CreateAuthZoneProofByIdsOutput {
            proof_id: self.create_auth_zone_proof_by_ids(&input.ids, input.resource_address)?,
        })
    }

    fn handle_drop_proof(
        &mut self,
        input: DropProofInput,
    ) -> Result<DropProofOutput, RuntimeError> {
        self.drop_proof(input.proof_id)?;

        Ok(DropProofOutput {})
    }

    fn handle_get_proof_amount(
        &mut self,
        input: GetProofAmountInput,
    ) -> Result<GetProofAmountOutput, RuntimeError> {
        let proof = self
            .proofs
            .get(&input.proof_id)
            .ok_or(RuntimeError::ProofNotFound(input.proof_id))?;

        Ok(GetProofAmountOutput {
            amount: proof.total_amount(),
        })
    }

    fn handle_get_proof_resource_address(
        &mut self,
        input: GetProofResourceAddressInput,
    ) -> Result<GetProofResourceAddressOutput, RuntimeError> {
        let proof = self
            .proofs
            .get(&input.proof_id)
            .ok_or(RuntimeError::ProofNotFound(input.proof_id))?;

        Ok(GetProofResourceAddressOutput {
            resource_address: proof.resource_address(),
        })
    }

    fn handle_get_non_fungible_ids_in_proof(
        &mut self,
        input: GetNonFungibleIdsInProofInput,
    ) -> Result<GetNonFungibleIdsInProofOutput, RuntimeError> {
        let proof = self
            .proofs
            .get(&input.proof_id)
            .ok_or(RuntimeError::ProofNotFound(input.proof_id))?;

        Ok(GetNonFungibleIdsInProofOutput {
            non_fungible_ids: proof
                .total_ids()
                .map_err(RuntimeError::ProofError)?
                .into_iter()
                .collect(),
        })
    }

    fn handle_clone_proof(
        &mut self,
        input: CloneProofInput,
    ) -> Result<CloneProofOutput, RuntimeError> {
        Ok(CloneProofOutput {
            proof_id: self.clone_proof(input.proof_id)?,
        })
    }

    fn handle_push_to_auth_zone(
        &mut self,
        input: PushToAuthZoneInput,
    ) -> Result<PushToAuthZoneOutput, RuntimeError> {
        self.push_to_auth_zone(input.proof_id)
            .map(|_| PushToAuthZoneOutput {})
    }

    fn handle_pop_from_auth_zone(
        &mut self,
        _input: PopFromAuthZoneInput,
    ) -> Result<PopFromAuthZoneOutput, RuntimeError> {
        self.pop_from_auth_zone()
            .map(|proof_id| PopFromAuthZoneOutput { proof_id })
    }

    fn handle_emit_log(&mut self, input: EmitLogInput) -> Result<EmitLogOutput, RuntimeError> {
        self.track.add_log(input.level, input.message);

        Ok(EmitLogOutput {})
    }

    fn handle_get_call_data(
        &mut self,
        _input: GetCallDataInput,
    ) -> Result<GetCallDataOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_ref()
            .ok_or(RuntimeError::InterpreterNotStarted)?;
        let component = match wasm_process.interpreter_state {
            InterpreterState::Component {
                component_address, ..
            } => Some(component_address.clone()),
            InterpreterState::Blueprint => None,
        };
        Ok(GetCallDataOutput {
            function: wasm_process.vm.function.clone(),
            component,
            arg: wasm_process.vm.arg.raw.clone(),
        })
    }

    fn handle_get_transaction_hash(
        &mut self,
        _input: GetTransactionHashInput,
    ) -> Result<GetTransactionHashOutput, RuntimeError> {
        Ok(GetTransactionHashOutput {
            transaction_hash: self.track.transaction_hash(),
        })
    }

    fn handle_get_current_epoch(
        &mut self,
        _input: GetCurrentEpochInput,
    ) -> Result<GetCurrentEpochOutput, RuntimeError> {
        Ok(GetCurrentEpochOutput {
            current_epoch: self.track.current_epoch(),
        })
    }

    fn handle_generate_uuid(
        &mut self,
        _input: GenerateUuidInput,
    ) -> Result<GenerateUuidOutput, RuntimeError> {
        Ok(GenerateUuidOutput {
            uuid: self.track.new_uuid(),
        })
    }

    fn handle_get_actor(&mut self, _input: GetActorInput) -> Result<GetActorOutput, RuntimeError> {
        let wasm_process = self
            .wasm_process_state
            .as_ref()
            .ok_or(RuntimeError::InterpreterNotStarted)?;

        return Ok(GetActorOutput {
            actor: wasm_process.vm.actor.clone(),
        });
    }

    //============================
    // SYSTEM CALL HANDLERS END
    //============================
}

impl<'r, 'l, L: SubstateStore> Externals for Process<'r, 'l, L> {
    fn invoke_index(
        &mut self,
        index: usize,
        args: RuntimeArgs,
    ) -> Result<Option<RuntimeValue>, Trap> {
        match index {
            ENGINE_FUNCTION_INDEX => {
                let operation: u32 = args.nth_checked(0)?;
                match operation {
                    PUBLISH_PACKAGE => self.handle(args, Self::handle_publish),
                    CALL_FUNCTION => self.handle(args, Self::handle_call_function),
                    CALL_METHOD => self.handle(args, Self::handle_call_method),

                    CREATE_COMPONENT => self.handle(args, Self::handle_create_component),
                    GET_COMPONENT_INFO => self.handle(args, Self::handle_get_component_info),
                    GET_COMPONENT_STATE => self.handle(args, Self::handle_get_component_state),
                    PUT_COMPONENT_STATE => self.handle(args, Self::handle_put_component_state),

                    CREATE_LAZY_MAP => self.handle(args, Self::handle_create_lazy_map),
                    GET_LAZY_MAP_ENTRY => self.handle(args, Self::handle_get_lazy_map_entry),
                    PUT_LAZY_MAP_ENTRY => self.handle(args, Self::handle_put_lazy_map_entry),

                    CREATE_RESOURCE => self.handle(args, Self::handle_create_resource),
                    GET_RESOURCE_TYPE => self.handle(args, Self::handle_get_resource_type),
                    GET_RESOURCE_METADATA => self.handle(args, Self::handle_get_resource_metadata),
                    GET_RESOURCE_TOTAL_SUPPLY => {
                        self.handle(args, Self::handle_get_resource_total_supply)
                    }
                    MINT_RESOURCE => self.handle(args, Self::handle_mint_resource),
                    BURN_RESOURCE => self.handle(args, Self::handle_burn_resource),
                    UPDATE_NON_FUNGIBLE_MUTABLE_DATA => {
                        self.handle(args, Self::handle_update_non_fungible_mutable_data)
                    }
                    GET_NON_FUNGIBLE_DATA => self.handle(args, Self::handle_get_non_fungible_data),
                    NON_FUNGIBLE_EXISTS => self.handle(args, Self::handle_non_fungible_exists),
                    UPDATE_RESOURCE_METADATA => {
                        self.handle(args, Self::handle_update_resource_metadata)
                    }

                    CREATE_EMPTY_VAULT => self.handle(args, Self::handle_create_vault),
                    PUT_INTO_VAULT => self.handle(args, Self::handle_put_into_vault),
                    TAKE_FROM_VAULT => self.handle(args, Self::handle_take_from_vault),
                    GET_VAULT_AMOUNT => self.handle(args, Self::handle_get_vault_amount),
                    GET_VAULT_RESOURCE_ADDRESS => {
                        self.handle(args, Self::handle_get_vault_resource_address)
                    }
                    TAKE_NON_FUNGIBLES_FROM_VAULT => {
                        self.handle(args, Self::handle_take_non_fungibles_from_vault)
                    }
                    GET_NON_FUNGIBLE_IDS_IN_VAULT => {
                        self.handle(args, Self::handle_get_non_fungible_ids_in_vault)
                    }

                    CREATE_EMPTY_BUCKET => self.handle(args, Self::handle_create_bucket),
                    PUT_INTO_BUCKET => self.handle(args, Self::handle_put_into_bucket),
                    TAKE_FROM_BUCKET => self.handle(args, Self::handle_take_from_bucket),
                    GET_BUCKET_AMOUNT => self.handle(args, Self::handle_get_bucket_amount),
                    GET_BUCKET_RESOURCE_ADDRESS => {
                        self.handle(args, Self::handle_get_bucket_resource_address)
                    }
                    TAKE_NON_FUNGIBLES_FROM_BUCKET => {
                        self.handle(args, Self::handle_take_non_fungibles_from_bucket)
                    }
                    GET_NON_FUNGIBLE_IDS_IN_BUCKET => {
                        self.handle(args, Self::handle_get_non_fungible_ids_in_bucket)
                    }

                    CREATE_BUCKET_PROOF => self.handle(args, Self::handle_create_bucket_proof),
                    CREATE_VAULT_PROOF => self.handle(args, Self::handle_create_vault_proof),
                    CREATE_VAULT_PROOF_BY_AMOUNT => {
                        self.handle(args, Self::handle_create_vault_proof_by_amount)
                    }
                    CREATE_VAULT_PROOF_BY_IDS => {
                        self.handle(args, Self::handle_create_vault_proof_by_ids)
                    }
                    CREATE_AUTH_ZONE_PROOF => {
                        self.handle(args, Self::handle_create_auth_zone_proof)
                    }
                    CREATE_AUTH_ZONE_PROOF_BY_AMOUNT => {
                        self.handle(args, Self::handle_create_auth_zone_proof_by_amount)
                    }
                    CREATE_AUTH_ZONE_PROOF_BY_IDS => {
                        self.handle(args, Self::handle_create_auth_zone_proof_by_ids)
                    }
                    DROP_PROOF => self.handle(args, Self::handle_drop_proof),
                    GET_PROOF_AMOUNT => self.handle(args, Self::handle_get_proof_amount),
                    GET_PROOF_RESOURCE_ADDRESS => {
                        self.handle(args, Self::handle_get_proof_resource_address)
                    }
                    GET_NON_FUNGIBLE_IDS_IN_PROOF => {
                        self.handle(args, Self::handle_get_non_fungible_ids_in_proof)
                    }
                    CLONE_PROOF => self.handle(args, Self::handle_clone_proof),
                    PUSH_TO_AUTH_ZONE => self.handle(args, Self::handle_push_to_auth_zone),
                    POP_FROM_AUTH_ZONE => self.handle(args, Self::handle_pop_from_auth_zone),

                    EMIT_LOG => self.handle(args, Self::handle_emit_log),
                    GET_CALL_DATA => self.handle(args, Self::handle_get_call_data),
                    GET_TRANSACTION_HASH => self.handle(args, Self::handle_get_transaction_hash),
                    GET_CURRENT_EPOCH => self.handle(args, Self::handle_get_current_epoch),
                    GENERATE_UUID => self.handle(args, Self::handle_generate_uuid),
                    GET_ACTOR => self.handle(args, Self::handle_get_actor),

                    _ => Err(RuntimeError::InvalidRequestCode(operation).into()),
                }
            }
            _ => Err(RuntimeError::HostFunctionNotFound(index).into()),
        }
    }
}
