#![no_std]
extern crate alloc;

pub mod traits;
pub use crate::traits::agent::Agent;

pub mod constants;

pub const API_VERSION: &str = "0.1.0";
