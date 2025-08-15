#![no_std]
extern crate alloc;
mod register_guest_function;
pub mod prelude;
pub use register_guest_function::register_guest_function;
pub use hyperlight_agents_common as agents_common;
pub use hyperlight_common as common;
pub use hyperlight_guest as guest;
pub use hyperlight_guest_bin as guest_bin;
pub use strum_macros;
//pub type Result<T> = core::result::Result<T, guest::error::HyperlightGuestError>;

use alloc::string::String;
use alloc::vec::Vec;
use core::clone::Clone;
use common::flatbuffer_wrappers::function_call::FunctionCall;
use common::flatbuffer_wrappers::function_types::ParameterValue;
use common::flatbuffer_wrappers::guest_error::ErrorCode;
use common::flatbuffer_wrappers::util::get_flatbuffer_result;
use guest::error::{HyperlightGuestError, Result};
use guest_bin::host_comm::call_host_function;
use core::result::Result::Err;
use core::result::Result::Ok;
use core::option::Option::Some;
use agents_common::structs::agent_message::AgentMessage;

/// Send a message to the host using a method name, guest message, and callback function.
pub fn send_message_to_host_method(
	method_name: &str,
	message: AgentMessage
) -> Result<Vec<u8>> {
	let serialized = serde_json::to_string(&message).unwrap();
	let _res = call_host_function::<String>(
		method_name,
		Some(Vec::from(&[
			ParameterValue::String(serialized)
		])),
		common::flatbuffer_wrappers::function_types::ReturnType::String,
	)?;
	Ok(get_flatbuffer_result("Success"))
}

/// Default guest_dispatch_function for guests that do not support dynamic dispatch.
pub fn default_guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
	Err(HyperlightGuestError::new(
		ErrorCode::GuestFunctionNotFound,
		function_call.function_name.clone(),
	))
}
