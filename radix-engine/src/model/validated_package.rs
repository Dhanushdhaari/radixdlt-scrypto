use sbor::rust::collections::HashMap;
use sbor::rust::string::String;
use sbor::rust::vec::Vec;
use sbor::*;
use scrypto::abi::BlueprintAbi;
use scrypto::buffer::scrypto_decode;
use scrypto::component::PackageFunction;
use scrypto::values::ScryptoValue;

use crate::engine::SystemApi;
use crate::wasm::*;

/// A collection of blueprints, compiled and published as a single unit.
#[derive(Debug, Clone, TypeId, Encode, Decode)]
pub struct ValidatedPackage {
    code: Vec<u8>,
    instrumented_code: Vec<u8>,
    blueprint_abis: HashMap<String, BlueprintAbi>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PackageError {
    InvalidRequestData(DecodeError),
    BlueprintNotFound,
    WasmValidationError(WasmValidationError),
    MethodNotFound(String),
}

impl ValidatedPackage {
    /// Validates and creates a package
    pub fn new(package: scrypto::prelude::Package) -> Result<Self, WasmValidationError> {
        let mut wasm_engine = WasmiEngine::new();
        wasm_engine.validate(&package.code)?;

        // instrument wasm
        let instrumented_code = wasm_engine
            .instrument(&package.code)
            .map_err(|_| WasmValidationError::FailedToInstrumentCode)?;

        Ok(Self {
            code: package.code,
            blueprint_abis: package.blueprint_abis,
            instrumented_code,
        })
    }

    pub fn code(&self) -> &[u8] {
        &self.code
    }

    pub fn instrumented_code(&self) -> &[u8] {
        &self.instrumented_code
    }

    pub fn blueprint_abi(
        &self,
        blueprint_name: &str,
    ) -> Option<&BlueprintAbi> {
        self.blueprint_abis.get(blueprint_name)
    }

    pub fn contains_blueprint(&self, blueprint_name: &str) -> bool {
        self.blueprint_abis.contains_key(blueprint_name)
    }

    pub fn load_blueprint_schema(&self, blueprint_name: &str) -> Result<&Type, PackageError> {
        self.blueprint_abi(blueprint_name)
            .map(|v| &v.value_schema)
            .ok_or(PackageError::BlueprintNotFound)
    }

    pub fn static_main<S: SystemApi>(
        call_data: ScryptoValue,
        system_api: &mut S,
    ) -> Result<ScryptoValue, PackageError> {
        let function: PackageFunction =
            scrypto_decode(&call_data.raw).map_err(|e| PackageError::InvalidRequestData(e))?;
        match function {
            PackageFunction::Publish(package) => {
                let package =
                    ValidatedPackage::new(package).map_err(PackageError::WasmValidationError)?;
                let package_address = system_api.create_package(package);
                Ok(ScryptoValue::from_value(&package_address))
            }
        }
    }
}
