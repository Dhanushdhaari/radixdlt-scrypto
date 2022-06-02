use sbor::rust::borrow::ToOwned;
use sbor::rust::vec::Vec;
use sbor::*;

use crate::buffer::{scrypto_decode, scrypto_encode};
use crate::call_data_bytes_args;
use crate::component::*;
use crate::core::*;
use crate::crypto::*;
use crate::engine::{api::*, call_engine};

#[derive(Debug, TypeId, Encode, Decode)]
pub enum SystemFunction {
    GetEpoch(),
    GetTransactionHash(),
}

/// The transaction runtime.
#[derive(Debug)]
pub struct Runtime {}

impl Runtime {
    /// Returns the running entity, a component if within a call-method context or a
    /// blueprint if within a call-function context.
    pub fn actor() -> ScryptoActorInfo {
        let input = RadixEngineInput::GetActor();
        let output: ScryptoActorInfo = call_engine(input);
        output
    }

    /// Returns the package ID.
    pub fn package_address() -> PackageAddress {
        let input = RadixEngineInput::GetActor();
        let output: ScryptoActorInfo = call_engine(input);
        output.to_package_address()
    }

    /// Generates a UUID.
    pub fn generate_uuid() -> u128 {
        let input = RadixEngineInput::GenerateUuid();
        let output: u128 = call_engine(input);

        output
    }

    /// Invokes a function on a blueprint.
    pub fn call_function<S: AsRef<str>>(
        package_address: PackageAddress,
        blueprint_name: S,
        function: S,
        args: Vec<Vec<u8>>,
    ) -> Vec<u8> {
        let call_data = call_data_bytes_args!(function.as_ref().to_owned(), args);
        let input = RadixEngineInput::InvokeSNode(
            SNodeRef::Scrypto(ScryptoActor::Blueprint(
                package_address,
                blueprint_name.as_ref().to_owned(),
            )),
            call_data,
        );
        let output: Vec<u8> = call_engine(input);

        output
    }

    /// Invokes a method on a component.
    pub fn call_method<S: AsRef<str>>(
        component_address: ComponentAddress,
        method: S,
        args: Vec<Vec<u8>>,
    ) -> Vec<u8> {
        let mut fields = Vec::new();
        for arg in args {
            fields.push(::sbor::decode_any(&arg).unwrap());
        }
        let variant = ::sbor::Value::Enum {
            name: method.as_ref().to_owned(),
            fields,
        };

        let input = RadixEngineInput::InvokeSNode(
            SNodeRef::Scrypto(ScryptoActor::Component(component_address)),
            ::sbor::encode_any(&variant),
        );
        let output: Vec<u8> = call_engine(input);

        output
    }

    /// Returns the transaction hash.
    pub fn transaction_hash() -> Hash {
        let input = RadixEngineInput::InvokeSNode(
            SNodeRef::SystemStatic,
            scrypto_encode(&SystemFunction::GetTransactionHash()),
        );
        let output: Vec<u8> = call_engine(input);
        scrypto_decode(&output).unwrap()
    }

    /// Returns the current epoch number.
    pub fn current_epoch() -> u64 {
        let input = RadixEngineInput::InvokeSNode(
            SNodeRef::SystemStatic,
            scrypto_encode(&SystemFunction::GetEpoch()),
        );
        let output: Vec<u8> = call_engine(input);
        scrypto_decode(&output).unwrap()
    }
}
