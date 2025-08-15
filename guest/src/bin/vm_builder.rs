#![no_std]
#![no_main]

extern crate alloc;
use alloc::collections::btree_map::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use hyperlight_agents_common::structs::agent_message::AgentMessage;
use hyperlight_agents_guest_common::prelude::*;
use serde::Deserialize;
use serde_json::{Map, Value};

pub const PROCESS_VM_CREATION_RESULT: &str = "ProcessVmCreationResult";
pub const PROCESS_VM_COMMAND_RESULT: &str = "ProcessVmCommandResult";
pub const PROCESS_VM_DESTRUCTION_RESULT: &str = "ProcessVmDestructionResult";
pub const PROCESS_VM_LIST_RESULT: &str = "ProcessVmListResult";

pub const PARAM_ACTION: &str = "action";
pub const PARAM_VM_ID: &str = "vm_id";
pub const PARAM_COMMAND: &str = "command";

#[derive(Deserialize, Debug)]
struct VmActionParams {
    #[serde(rename = "action")]
    action: String,
    #[serde(rename = "vm_id")]
    vm_id: Option<String>,
    #[serde(rename = "command")]
    command: Option<String>,
}

fn guest_run(function_call: &FunctionCall) -> Result<Vec<u8>> {
    match function_call.parameters.as_ref().and_then(|p| p.get(0)) {
        Some(ParameterValue::String(json_params)) => {
            let params: VmActionParams = match serde_json::from_str(json_params) {
                Ok(p) => p,
                Err(_) => {
                    return Err(HyperlightGuestError::new(
                        ErrorCode::GuestFunctionParameterTypeMismatch,
                        "Failed to parse VM action parameters".to_string(),
                    ))
                }
            };
            let action = params.action;
            let vm_id = params.vm_id.unwrap_or_else(|| "default_vm".to_string());
            let command = params.command.unwrap_or_default();
            let res = match action.as_str() {
                "create_vm" => call_host_function::<String>(
                    constants::HostMethod::CreateVM.as_ref(),
                    Some(vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(PROCESS_VM_CREATION_RESULT.to_string()),
                    ]),
                    ReturnType::String,
                ),
                "execute_vm_command" => call_host_function::<String>(
                    constants::HostMethod::ExecuteVMCommand.as_ref(),
                    Some(vec![
                        ParameterValue::String(vm_id.clone()),
                        ParameterValue::String(command.clone()),
                        ParameterValue::String(PROCESS_VM_COMMAND_RESULT.to_string()),
                    ]),
                    ReturnType::String,
                ),
                "spawn_command" => call_host_function::<String>(
                    constants::HostMethod::SpawnCommand.as_ref(),
                    Some(vec![
                        ParameterValue::String(vm_id.clone()),
                        ParameterValue::String(command.clone()),
                        ParameterValue::String(PROCESS_VM_COMMAND_RESULT.to_string()),
                    ]),
                    ReturnType::String,
                ),
                "list_spawned_processes" => call_host_function::<String>(
                    constants::HostMethod::ListSpawnedProcesses.as_ref(),
                    Some(vec![
                        ParameterValue::String(vm_id.clone()),
                        ParameterValue::String(PROCESS_VM_LIST_RESULT.to_string()),
                    ]),
                    ReturnType::String,
                ),
                "stop_spawned_process" => call_host_function::<String>(
                    constants::HostMethod::StopSpawnedProcess.as_ref(),
                    Some(vec![
                        ParameterValue::String(vm_id.clone()),
                        ParameterValue::String(command.clone()),
                        ParameterValue::String(PROCESS_VM_COMMAND_RESULT.to_string()),
                    ]),
                    ReturnType::String,
                ),
                "destroy_vm" => call_host_function::<String>(
                    constants::HostMethod::DestroyVM.as_ref(),
                    Some(vec![
                        ParameterValue::String(vm_id),
                        ParameterValue::String(PROCESS_VM_DESTRUCTION_RESULT.to_string()),
                    ]),
                    ReturnType::String,
                ),
                "list_vms" => call_host_function::<String>(
                    constants::HostMethod::ListVMs.as_ref(),
                    Some(vec![
                        ParameterValue::String("".to_string()),
                        ParameterValue::String(PROCESS_VM_LIST_RESULT.to_string()),
                    ]),
                    ReturnType::String,
                ),
                _ => return Err(HyperlightGuestError::new(
                    ErrorCode::GuestFunctionParameterTypeMismatch,
                    format!("VM action invalid, must be one of: create_vm, execute_vm_command, spawn_command, list_spawned_processes, stop_spawned_process, destroy_vm, list_vms. Got {:?}", action),
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
        }
        _ => Err(HyperlightGuestError::new(
            ErrorCode::GuestFunctionParameterTypeMismatch,
            "VM action invalid, expected string parameter".to_string(),
        )),
    }
}

fn get_mcp_tool(_function_call: &FunctionCall) -> Result<Vec<u8>> {
    let mut params = BTreeMap::new();

    let mut action_schema = Map::new();
    action_schema.insert("type".to_string(), Value::String("string".to_string()));
    action_schema.insert("description".to_string(), Value::String("Action to perform, must be one of: create_vm, execute_vm_command, spawn_command, list_spawned_processes, stop_spawned_process, destroy_vm, list_vms".to_string()));
    params.insert(PARAM_ACTION.to_string(), action_schema);

    let mut vm_id_schema = Map::new();
    vm_id_schema.insert("type".to_string(), Value::String("string".to_string()));
    vm_id_schema.insert(
        "description".to_string(),
        Value::String("ID of the VM to operate on".to_string()),
    );
    params.insert(PARAM_VM_ID.to_string(), vm_id_schema);

    let mut command_schema = Map::new();
    command_schema.insert("type".to_string(), Value::String("string".to_string()));
    command_schema.insert("description".to_string(), Value::String("Command to execute in the VM, arguments for spawn_command, or process_id for stop_spawned_process".to_string()));
    params.insert(PARAM_COMMAND.to_string(), command_schema);

    let required = vec![PARAM_ACTION.to_string(), PARAM_VM_ID.to_string()];

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

fn process_result(function_call: &FunctionCall, label: &str) -> Result<Vec<u8>> {
    match function_call.parameters.as_ref().and_then(|p| p.get(0)) {
        Some(ParameterValue::String(response)) => {
            let message = AgentMessage {
                callback: None,
                message: Some(response.clone()),
                guest_message: Some(label.to_string()),
                is_success: true,
            };
            send_message_to_host_method(constants::HostMethod::FinalResult.as_ref(), message)
        }
        _ => Ok(get_flatbuffer_result(format!("{label} processed").as_str())),
    }
}

fn process_vm_creation_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    process_result(function_call, "VM Creation Result")
}
fn process_vm_command_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    process_result(function_call, "VM Command Result")
}
fn process_vm_destruction_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    process_result(function_call, "VM Destruction Result")
}

fn process_vm_list_result(function_call: &FunctionCall) -> Result<Vec<u8>> {
    // For list, show all parameters
    if let Some(parameters) = function_call.parameters.as_ref() {
        let result_message = format!("Available VMs: {:?}", parameters);
        let message = AgentMessage {
            callback: None,
            message: Some(result_message),
            guest_message: None,
            is_success: true,
        };
        return send_message_to_host_method(constants::HostMethod::FinalResult.as_ref(), message);
    }
    Ok(get_flatbuffer_result("VM list result processed"))
}

#[no_mangle]
pub extern "C" fn hyperlight_main() {
    register_guest_function(
        constants::GuestMethod::Run.as_ref(),
        &[ParameterType::String],
        ReturnType::String,
        guest_run as usize,
    );
    register_guest_function(
        constants::GuestMethod::GetMCPTool.as_ref(),
        &[],
        ReturnType::String,
        get_mcp_tool as usize,
    );
    // Register callback functions
    register_guest_function(
        PROCESS_VM_CREATION_RESULT,
        &[ParameterType::String],
        ReturnType::String,
        process_vm_creation_result as usize,
    );
    register_guest_function(
        PROCESS_VM_COMMAND_RESULT,
        &[ParameterType::String],
        ReturnType::String,
        process_vm_command_result as usize,
    );
    register_guest_function(
        PROCESS_VM_DESTRUCTION_RESULT,
        &[ParameterType::String],
        ReturnType::String,
        process_vm_destruction_result as usize,
    );
    register_guest_function(
        PROCESS_VM_LIST_RESULT,
        &[ParameterType::String],
        ReturnType::String,
        process_vm_list_result as usize,
    );
}

#[no_mangle]
pub fn guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
    hyperlight_agents_guest_common::default_guest_dispatch_function(function_call)
}
