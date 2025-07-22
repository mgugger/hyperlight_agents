#![no_std]
extern crate alloc;

pub mod traits;
pub use crate::traits::agent::Agent;

pub mod structs;
pub use crate::structs::mcp_tool::{
    Annotations, Role, Tool, ToolAnnotations, ToolInputSchema, ToolOutputSchema,
};

pub mod constants;

pub const API_VERSION: &str = "0.1.0";

use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmCommand {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub timeout_seconds: Option<u64>,
    pub mode: VmCommandMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VmCommandMode {
    Foreground,
    Spawn,
    // Add more modes as needed
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmCommandResult {
    pub id: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}
