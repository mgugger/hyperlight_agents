#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;
use hyperlight_agents_common::constants;

use hyperlight_agents_common::Agent;
use hyperlight_common::flatbuffer_wrappers::function_call::FunctionCall;
use hyperlight_common::flatbuffer_wrappers::function_types::{
    ParameterType, ParameterValue, ReturnType,
};
use hyperlight_common::flatbuffer_wrappers::guest_error::ErrorCode;
use hyperlight_common::flatbuffer_wrappers::util::get_flatbuffer_result;
use hyperlight_guest::error::{HyperlightGuestError, Result};
use hyperlight_guest::guest_function_definition::GuestFunctionDefinition;
use hyperlight_guest::guest_function_register::register_function;
use hyperlight_guest::host_function_call::call_host_function;
use strum_macros::AsRefStr;

pub struct VmBuilderAgent;

#[derive(Debug, PartialEq, AsRefStr)]
enum AgentConstants {
    ProcessVmCreationResult,
    ProcessVmCommandResult,
    ProcessVmDestructionResult,
    ProcessVmListResult,
}

impl Agent for VmBuilderAgent {
    type Error = HyperlightGuestError;

    fn get_name() -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        Ok(get_flatbuffer_result("VmBuilder"))
    }

    fn get_description() -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        Ok(get_flatbuffer_result(
            "An Agent that can create VMs and execute build/test commands in them",
        ))
    }

    fn get_params() -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        let params_json = r#"[
            {
                "name": "action",
                "description": "Action to perform, must be one of: create_vm, execute_vm_command, destroy_vm, list_vms",
                "type": "string",
                "required": true
            },
            {
                "name": "vm_id",
                "description": "ID of the VM to operate on",
                "type": "string",
                "required": false
            },
            {
                "name": "command",
                "description": "Command to execute in the VM",
                "type": "string",
                "required": false
            }
        ]"#;
        Ok(get_flatbuffer_result(params_json))
    }

    fn process(
        function_call: &FunctionCall,
    ) -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        if let ParameterValue::String(json_params) = &function_call.parameters.as_ref().unwrap()[0]
        {
            let action =
                parse_json_param(json_params, "action").unwrap_or_else(|| "create_vm".to_string());
            let vm_id =
                parse_json_param(json_params, "vm_id").unwrap_or_else(|| "default_vm".to_string());
            let command =
                parse_json_param(json_params, "command").unwrap_or_else(|| "".to_string());

            let res: Result<()> = match action.as_str() {
            "create_vm" => {
                let params = Vec::from(&[
                    ParameterValue::String(vm_id),
                    ParameterValue::String(
                    AgentConstants::ProcessVmCreationResult.as_ref().to_string(),
                ),
                ]);
                call_host_function(
                    constants::HostMethod::CreateVM.as_ref(),
                    Some(params),
                    ReturnType::String,
                )
            },
            "execute_vm_command" => {
                let params = Vec::from(&[
                    ParameterValue::String(vm_id),
                    ParameterValue::String(command),
                    ParameterValue::String(
                    AgentConstants::ProcessVmCommandResult.as_ref().to_string(),
                ),
                ]);
                call_host_function(
                    constants::HostMethod::ExecuteVMCommand.as_ref(),
                    Some(params),
                    ReturnType::String,
                )
            },
            "destroy_vm" => {
                let params = Vec::from(&[ParameterValue::String(vm_id),
                ParameterValue::String(
                    AgentConstants::ProcessVmDestructionResult.as_ref().to_string(),
                )]);
                call_host_function(
                    constants::HostMethod::DestroyVM.as_ref(),
                    Some(params),
                    ReturnType::String,
                )
            },
            "list_vms" => {
                let params = Vec::from(&[ParameterValue::String("".to_string()),
                ParameterValue::String(
                    AgentConstants::ProcessVmListResult.as_ref().to_string(),
                )]);
                call_host_function(constants::HostMethod::ListVMs.as_ref(), Some(params), ReturnType::String)},
            _ => return Err(HyperlightGuestError::new(
                ErrorCode::GuestFunctionParameterTypeMismatch,
                format!("VM action invalid, must be one of: create_vm, execute_vm_command, destroy_vm, list_vms. Got {:?}", action).to_string(),
            )),
        };
            match res {
                Ok(_) => Ok(get_flatbuffer_result(
                    format!(
                        "VM operation OK: {:?}",
                        action
                    )
                    .as_str(),
                )),
                Err(e) => Ok(get_flatbuffer_result(
                    format!("VM operation failed {:?}", e).as_str(),
                )),
            }
        } else {
            Err(HyperlightGuestError::new(
            ErrorCode::GuestFunctionParameterTypeMismatch,
            "VM action invalid, must be one of: create_vm, execute_vm_command, destroy_vm, list_vms".to_string(),
        ))
        }
    }
}

fn parse_json_param(json: &str, key: &str) -> Option<alloc::string::String> {
    let pattern = format!("\"{}\":\"", key);
    if let Some(start) = json.find(&pattern) {
        let start = start + pattern.len();
        if let Some(end) = json[start..].find("\"") {
            Some(json[start..start + end].to_string())
        } else {
            None
        }
    } else {
        None
    }
}

fn send_message_to_host_method(
    method_name: &str,
    guest_message: &str,
    message: &str,
    callback_function: &str,
) -> core::result::Result<Vec<u8>, HyperlightGuestError> {
    let message = format!("{}{}", guest_message, message);
    call_host_function(
        method_name,
        Some(Vec::from(&[
            ParameterValue::String(message),
            ParameterValue::String(callback_function.to_string()),
        ])),
        ReturnType::String,
    )?;

    Ok(get_flatbuffer_result(
        format!("Host function {} called successfully", method_name).as_str(),
    ))
}

fn process_vm_creation_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    if let Some(parameters) = &function_call.parameters {
        if parameters.len() > 0 {
            if let Some(param) = parameters.get(0) {
                if let ParameterValue::String(response) = param {
                    let result_message = format!("VM Creation Result: {}", response);
                    return send_message_to_host_method(
                        constants::HostMethod::FinalResult.as_ref(),
                        result_message.as_str(),
                        "",
                        "",
                    );
                }
            }
        }
    }
    Ok(get_flatbuffer_result("VM creation result processed"))
}

fn process_vm_command_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    if let Some(parameters) = &function_call.parameters {
        if parameters.len() > 0 {
            if let Some(param) = parameters.get(0) {
                if let ParameterValue::String(response) = param {
                    let result_message = format!("VM Command Result: {}", response);
                    return send_message_to_host_method(
                        constants::HostMethod::FinalResult.as_ref(),
                        result_message.as_str(),
                        "",
                        "",
                    );
                }
            }
        }
    }
    Ok(get_flatbuffer_result("VM command result processed"))
}

fn process_vm_destruction_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    if let Some(parameters) = &function_call.parameters {
        if parameters.len() > 0 {
            if let Some(param) = parameters.get(0) {
                if let ParameterValue::String(response) = param {
                    let result_message = format!("VM Destruction Result: {}", response);
                    return send_message_to_host_method(
                        constants::HostMethod::FinalResult.as_ref(),
                        result_message.as_str(),
                        "",
                        "",
                    );
                }
            }
        }
    }
    Ok(get_flatbuffer_result("VM destruction result processed"))
}

fn process_vm_list_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    if let Some(parameters) = &function_call.parameters {
        if parameters.len() > 0 {
            let result_message = format!("Available VMs: {:?}", parameters);
            return send_message_to_host_method(
                constants::HostMethod::FinalResult.as_ref(),
                result_message.as_str(),
                "",
                "",
            );
        }
    }
    Ok(get_flatbuffer_result("VM list result processed"))
}

#[unsafe(no_mangle)]
pub extern "C" fn hyperlight_main() {
    let vm_builder_def = GuestFunctionDefinition::new(
        constants::GuestMethod::Run.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        VmBuilderAgent::process as usize,
    );
    register_function(vm_builder_def);

    let process_vm_creation_result_def = GuestFunctionDefinition::new(
        AgentConstants::ProcessVmCreationResult.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_creation_result as usize,
    );
    register_function(process_vm_creation_result_def);

    let process_vm_command_result_def = GuestFunctionDefinition::new(
        AgentConstants::ProcessVmCommandResult.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_command_result as usize,
    );
    register_function(process_vm_command_result_def);

    let process_vm_destruction_result_def = GuestFunctionDefinition::new(
        AgentConstants::ProcessVmDestructionResult
            .as_ref()
            .to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_destruction_result as usize,
    );
    register_function(process_vm_destruction_result_def);

    let process_vm_list_result_def = GuestFunctionDefinition::new(
        AgentConstants::ProcessVmListResult.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_list_result as usize,
    );
    register_function(process_vm_list_result_def);

    let get_name_def = GuestFunctionDefinition::new(
        constants::GuestMethod::GetName.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        VmBuilderAgent::get_name as usize,
    );
    register_function(get_name_def);

    let get_description_def = GuestFunctionDefinition::new(
        constants::GuestMethod::GetDescription.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        VmBuilderAgent::get_description as usize,
    );
    register_function(get_description_def);

    let get_params_def = GuestFunctionDefinition::new(
        constants::GuestMethod::GetParams.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        VmBuilderAgent::get_params as usize,
    );
    register_function(get_params_def);
}

#[unsafe(no_mangle)]
pub fn guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
    Err(HyperlightGuestError::new(
        ErrorCode::GuestFunctionNotFound,
        function_call.function_name.clone(),
    ))
}
