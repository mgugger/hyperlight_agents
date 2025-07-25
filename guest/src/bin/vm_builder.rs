#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::btree_map::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use hyperlight_agents_common::{constants, Tool, ToolInputSchema};
use hyperlight_common::flatbuffer_wrappers::function_call::FunctionCall;
use hyperlight_common::flatbuffer_wrappers::function_types::{
    ParameterType, ParameterValue, ReturnType,
};
use hyperlight_common::flatbuffer_wrappers::guest_error::ErrorCode;
use hyperlight_common::flatbuffer_wrappers::util::get_flatbuffer_result;
use hyperlight_guest::error::{HyperlightGuestError, Result};
use hyperlight_guest_bin::guest_function::definition::GuestFunctionDefinition;
use hyperlight_guest_bin::guest_function::register::register_function;
use hyperlight_guest_bin::host_comm::call_host_function;
use serde_json::{Map, Value};
use strum_macros::AsRefStr;

#[derive(Debug, PartialEq, AsRefStr)]
enum AgentConstants {
    ProcessVmCreationResult,
    ProcessVmCommandResult,
    ProcessVmDestructionResult,
    ProcessVmListResult,
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

fn guest_run(function_call: &FunctionCall) -> Result<Vec<u8>> {
    if let Some(parameters) = &function_call.parameters {
        if let Some(ParameterValue::String(json_params)) = parameters.get(0) {
            let action =
                parse_json_param(json_params, "action").unwrap_or_else(|| "create_vm".to_string());
            let vm_id =
                parse_json_param(json_params, "vm_id").unwrap_or_else(|| "default_vm".to_string());
            let command =
                parse_json_param(json_params, "command").unwrap_or_else(|| "".to_string());

            let res: Result<String> = match action.as_str() {
                "create_vm" => {
                    let params = vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(
                            AgentConstants::ProcessVmCreationResult.as_ref().to_string(),
                        ),
                    ];
                    call_host_function::<String>(
                        constants::HostMethod::CreateVM.as_ref(),
                        Some(params),
                        ReturnType::String,
                    )
                },
                "execute_vm_command" => {
                    let params = vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(command),
                        ParameterValue::String(
                            AgentConstants::ProcessVmCommandResult.as_ref().to_string(),
                        ),
                    ];
                    call_host_function::<String>(
                        constants::HostMethod::ExecuteVMCommand.as_ref(),
                        Some(params),
                        ReturnType::String,
                    )
                },
                "spawn_command" => {
                    let params = vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(command),
                        ParameterValue::String(
                            AgentConstants::ProcessVmCommandResult.as_ref().to_string(),
                        ),
                    ];
                    call_host_function::<String>(
                        constants::HostMethod::SpawnCommand.as_ref(),
                        Some(params),
                        ReturnType::String,
                    )
                },
                "list_spawned_processes" => {
                    let params = vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(
                            AgentConstants::ProcessVmListResult.as_ref().to_string(),
                        ),
                    ];
                    call_host_function::<String>(
                        constants::HostMethod::ListSpawnedProcesses.as_ref(),
                        Some(params),
                        ReturnType::String,
                    )
                },
                "stop_spawned_process" => {
                    let params = vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(command), // command here is process_id
                        ParameterValue::String(
                            AgentConstants::ProcessVmCommandResult.as_ref().to_string(),
                        ),
                    ];
                    call_host_function::<String>(
                        constants::HostMethod::StopSpawnedProcess.as_ref(),
                        Some(params),
                        ReturnType::String,
                    )
                },
                "destroy_vm" => {
                    let params = vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(
                            AgentConstants::ProcessVmDestructionResult.as_ref().to_string(),
                        )
                    ];
                    call_host_function::<String>(
                        constants::HostMethod::DestroyVM.as_ref(),
                        Some(params),
                        ReturnType::String,
                    )
                },
                "list_vms" => {
                    let params = vec![
                        ParameterValue::String("".to_string()),
                        ParameterValue::String(
                            AgentConstants::ProcessVmListResult.as_ref().to_string(),
                        )
                    ];
                    call_host_function::<String>(
                        constants::HostMethod::ListVMs.as_ref(),
                        Some(params),
                        ReturnType::String
                    )
                },
                _ => return Err(HyperlightGuestError::new(
                    ErrorCode::GuestFunctionParameterTypeMismatch,
                    format!("VM action invalid, must be one of: create_vm, execute_vm_command, spawn_command, list_spawned_processes, stop_spawned_process, destroy_vm, list_vms. Got {:?}", action).to_string(),
                )),
            };

            match res {
                Ok(response) => Ok(get_flatbuffer_result(
                    format!("VM operation OK: {:?} - {}", action, response).as_str(),
                )),
                Err(e) => Ok(get_flatbuffer_result(
                    format!("VM operation failed {:?}", e).as_str(),
                )),
            }
        } else {
            Err(HyperlightGuestError::new(
                ErrorCode::GuestFunctionParameterTypeMismatch,
                "VM action invalid, expected string parameter".to_string(),
            ))
        }
    } else {
        Err(HyperlightGuestError::new(
            ErrorCode::GuestFunctionParameterTypeMismatch,
            "VM action invalid, no parameters provided".to_string(),
        ))
    }
}

fn get_mcp_tool(_function_call: &FunctionCall) -> Result<Vec<u8>> {
    let mut params = BTreeMap::new();

    let mut action_schema = Map::new();
    action_schema.insert("type".to_string(), Value::String("string".to_string()));
    action_schema.insert("description".to_string(), Value::String("Action to perform, must be one of: create_vm, execute_vm_command, spawn_command, list_spawned_processes, stop_spawned_process, destroy_vm, list_vms".to_string()));
    params.insert("action".to_string(), action_schema);

    let mut vm_id_schema = Map::new();
    vm_id_schema.insert("type".to_string(), Value::String("string".to_string()));
    vm_id_schema.insert(
        "description".to_string(),
        Value::String("ID of the VM to operate on".to_string()),
    );
    params.insert("vm_id".to_string(), vm_id_schema);

    let mut command_schema = Map::new();
    command_schema.insert("type".to_string(), Value::String("string".to_string()));
    command_schema.insert("description".to_string(), Value::String("Command to execute in the VM, arguments for spawn_command, or process_id for stop_spawned_process".to_string()));
    params.insert("command".to_string(), command_schema);

    let required = vec!["action".to_string(), "vm_id".to_string()];

    let tool = Tool {
        name: "VmBuilder".to_string(),
        description: Some(
            "An Agent that can create VMs and execute build/test commands in them".to_string(),
        ),
        annotations: None,
        input_schema: ToolInputSchema::new(required, Some(params)),
        output_schema: None,
        title: None,
        meta: None,
    };
    let serialized = serde_json::to_string(&tool).unwrap();

    Ok(get_flatbuffer_result(serialized.as_str()))
}

fn send_message_to_host_method(
    method_name: &str,
    guest_message: &str,
    message: &str,
    callback_function: &str,
) -> Result<Vec<u8>> {
    let message = format!("{}{}", guest_message, message);

    let _res = call_host_function::<()>(
        method_name,
        Some(Vec::from(&[
            ParameterValue::String(message.to_string()),
            ParameterValue::String(callback_function.to_string()),
        ])),
        ReturnType::Void,
    )?;

    Ok(get_flatbuffer_result("Success"))
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
                    let result_message = format!("{}", response);
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

#[no_mangle]
pub extern "C" fn hyperlight_main() {
    // Register the main run function
    register_function(GuestFunctionDefinition::new(
        constants::GuestMethod::Run.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        guest_run as usize,
    ));

    register_function(GuestFunctionDefinition::new(
        constants::GuestMethod::GetMCPTool.as_ref().to_string(),
        Vec::new(),
        ReturnType::String,
        get_mcp_tool as usize,
    ));

    // Register callback functions
    register_function(GuestFunctionDefinition::new(
        AgentConstants::ProcessVmCreationResult.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_creation_result as usize,
    ));

    register_function(GuestFunctionDefinition::new(
        AgentConstants::ProcessVmCommandResult.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_command_result as usize,
    ));

    register_function(GuestFunctionDefinition::new(
        AgentConstants::ProcessVmDestructionResult
            .as_ref()
            .to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_destruction_result as usize,
    ));

    register_function(GuestFunctionDefinition::new(
        AgentConstants::ProcessVmListResult.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_vm_list_result as usize,
    ));
}

#[no_mangle]
pub fn guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
    Err(HyperlightGuestError::new(
        ErrorCode::GuestFunctionNotFound,
        function_call.function_name.clone(),
    ))
}
