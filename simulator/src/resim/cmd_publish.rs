use clap::Parser;
use colored::*;
use radix_engine::transaction::*;
use radix_engine::wasm::*;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

use crate::ledger::*;
use crate::resim::*;
use crate::utils::*;

/// Publish a package
#[derive(Parser, Debug)]
pub struct Publish {
    /// the path to a Scrypto package or a .wasm file
    path: PathBuf,

    /// Output a transaction manifest without execution
    #[clap(short, long)]
    manifest: Option<PathBuf>,

    /// Turn on tracing
    #[clap(short, long)]
    trace: bool,
}

impl Publish {
    pub fn run<O: std::io::Write>(&self, out: &mut O) -> Result<(), Error> {
        // Load wasm code
        let code = fs::read(if self.path.extension() != Some(OsStr::new("wasm")) {
            build_package(&self.path, false).map_err(Error::CargoError)?
        } else {
            self.path.clone()
        })
        .map_err(Error::IOError)?;

        if let Some(path) = &self.manifest {
            let package = extract_package(code).unwrap();
            let transaction = TransactionBuilder::new()
                .publish_package(package)
                .build_with_no_nonce();

            let mut substate_store = RadixEngineDB::with_bootstrap(get_data_dir()?);
            let mut wasm_engine = default_wasm_engine();
            let mut executor =
                TransactionExecutor::new(&mut substate_store, &mut wasm_engine, self.trace);
            process_transaction(&mut executor, transaction, &None, &Some(path.clone()), out)?;
        } else {
            self.store_package(out, code)?;
        }
        Ok(())
    }

    pub fn publish_wasm<O: std::io::Write>(
        &self,
        out: &mut O,
        wasm_file_path: &str,
    ) -> Result<(), Error> {
        // Load wasm code
        println!("Publishing ..");
        let code = fs::read(wasm_file_path).map_err(Error::IOError)?;
        println!("Read code to variable");
        self.store_package(out, code)
    }

    pub fn store_package<O: std::io::Write>(
        &self,
        out: &mut O,
        code: Vec<u8>,
    ) -> Result<(), Error> {
        let mut substate_store = RadixEngineDB::with_bootstrap(get_data_dir()?);
        let mut wasm_engine = default_wasm_engine();
        let mut executor =
            TransactionExecutor::new(&mut substate_store, &mut wasm_engine, self.trace);
        let package = extract_package(code).map_err(Error::PackageError)?;
        match executor.publish_package(package) {
            Ok(package_address) => {
                writeln!(
                    out,
                    "Success! New Package: {}",
                    package_address.to_string().green()
                )
                .map_err(Error::IOError)?;
                Ok(())
            }

            Err(error) => {
                writeln!(out, "Error creating new package: {:?}", error).map_err(Error::IOError)?;
                Err(Error::TransactionExecutionError(error))
            }
        }
    }
}
