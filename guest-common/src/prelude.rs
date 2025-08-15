//! guest-common prelude: import most-used items for guest agents
pub use crate::agents_common::{self, constants, Tool, ToolInputSchema};
pub use crate::common::flatbuffer_wrappers::function_call::FunctionCall;
pub use crate::common::flatbuffer_wrappers::function_types::{ParameterType, ParameterValue, ReturnType};
pub use crate::common::flatbuffer_wrappers::guest_error::ErrorCode;
pub use crate::common::flatbuffer_wrappers::util::get_flatbuffer_result;
pub use crate::guest::error::HyperlightGuestError;
pub use crate::register_guest_function;
pub use crate::send_message_to_host_method;
pub use crate::default_guest_dispatch_function;
pub use crate::guest_bin::host_comm::call_host_function;
pub type Result<T> = core::result::Result<T, crate::guest::error::HyperlightGuestError>;
