use alloc::{string::String, vec::Vec};
use hyperlight_common::flatbuffer_wrappers::function_call::FunctionCall;

pub trait Agent {
    type Error;

    fn get_name() -> Result<Vec<u8>, Self::Error>;
    fn get_description() -> Result<Vec<u8>, Self::Error>;
    fn get_params() -> Result<Vec<u8>, Self::Error>;
    fn process(function_call: &FunctionCall) -> Result<Vec<u8>, Self::Error>;
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub param_type: ParamType,
    pub description: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub enum ParamType {
    String,
    Integer,
    Boolean,
    Float,
}
