//! Revision-bound workflow design. The source remains the executable contract;
//! these records explain it, bind native model settings, and qualify its outputs.
pub mod access;
pub mod benchmark;
pub mod benchmark_import;
pub mod capabilities;
#[cfg(test)]
pub(crate) mod capabilities_tests;
pub mod contracts;
pub mod model;
pub mod profile_contracts;
pub mod profiles;
pub mod recovery;
#[cfg(test)]
pub(crate) mod recovery_tests;
pub mod repository;
pub mod research;
pub mod store;
#[cfg(test)]
mod tests;
pub mod validation;

pub use model::*;

pub fn digest(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value))
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
