use alloc::vec::Vec;
use hyperlight_common::flatbuffer_wrappers::function_call::FunctionCall;

pub trait Agent {
    type Error;

    fn get_name(&self) -> Result<Vec<u8>, Self::Error>;
    fn get_description(&self) -> Result<Vec<u8>, Self::Error>;
    fn get_params(&self) -> Result<Vec<u8>, Self::Error>;
    fn process(&self, function_call: &FunctionCall) -> Result<Vec<u8>, Self::Error>;
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Vec<u8>,
    pub param_type: ParamType,
    pub description: Option<Vec<u8>>,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub enum ParamType {
    String,
    Integer,
    Boolean,
    Float,
}
