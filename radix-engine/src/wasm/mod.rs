mod constants;
mod errors;
mod prepare;
mod traits;
#[cfg(not(feature = "alloc"))]
mod wasmer;
mod wasmi;

#[cfg(not(feature = "alloc"))]
pub use self::wasmer::*;
pub use self::wasmi::*;
pub use constants::*;
pub use errors::*;
pub use prepare::*;
pub use traits::*;

#[cfg(feature = "wasmer")]
pub fn default_wasm_engine() -> WasmerEngine {
    WasmerEngine::new()
}

#[cfg(not(feature = "wasmer"))]
pub fn default_wasm_engine() -> WasmiEngine {
    WasmiEngine::new()
}
