//! Helper for registering guest functions with less boilerplate.
use crate::guest_bin::guest_function::definition::GuestFunctionDefinition;
use crate::guest_bin::guest_function::register::register_function;
use crate::common::flatbuffer_wrappers::function_types::{ParameterType, ReturnType};
use crate::alloc::string::ToString;

/// Registers a guest function with the given name, parameter types, return type, and function pointer.
pub fn register_guest_function(
    name: &str,
    params: &[ParameterType],
    ret: ReturnType,
    func: usize,
) {
    register_function(GuestFunctionDefinition::new(
        name.to_string(),
        params.to_vec(),
        ret,
        func,
    ));
}
