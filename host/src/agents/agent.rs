use std::sync::{Arc, Mutex};
use std::time::Duration;

use hyperlight_common::flatbuffer_wrappers::function_types::{ParameterValue, ReturnType};
use hyperlight_host::func::{HostFunction2, ReturnValue};
use hyperlight_host::sandbox::SandboxConfiguration;
use hyperlight_host::sandbox_state::sandbox::EvolvableSandbox;
use hyperlight_host::sandbox_state::transition::Noop;
use hyperlight_host::{MultiUseSandbox, UninitializedSandbox};

use crate::host_functions::firecracker_vm_functions::VmManager;
use crate::host_functions::network_functions::http_request;
use crate::mcp_server::{MCP_AGENT_REQUEST_IDS, MCP_RESPONSE_CHANNELS};
use hyperlight_agents_common::constants;
use reqwest::Client;
use std::sync::mpsc::{Receiver, Sender, channel};

use hyperlight_agents_common::traits::agent::Param;

pub struct Agent {
    pub id: String,
    pub name: String,
    pub description: String,
    pub params: Vec<Param>,
    pub sandbox: MultiUseSandbox,
    pub tx: Sender<(Option<String>, String)>,
    pub rx: Receiver<(Option<String>, String)>, // (response, callback_name)
    pub request_id: Option<String>,             // For tracking MCP request IDs
}

pub fn create_agent(
    agent_id: String,
    http_client: Arc<Client>,
    binary_path: String,
    vm_manager: Arc<VmManager>,
) -> hyperlight_host::Result<Agent> {
    // Create a channel for communication
    let (tx, rx) = channel::<(Option<String>, String)>();

    // Create sandbox configuration
    let mut sandbox_config = SandboxConfiguration::default();
    sandbox_config.set_input_data_size(100 * 1024 * 1024);
    sandbox_config.set_output_data_size(100 * 1024 * 1024);
    sandbox_config.set_heap_size(100 * 1024 * 1024);

    // Create a sandbox for this agent
    let guest_instance = hyperlight_host::GuestBinary::FilePath(binary_path);

    let mut uninitialized_sandbox =
        UninitializedSandbox::new(guest_instance, Some(sandbox_config), None, None)?;

    // Register host functions specific to this agent
    register_host_functions(
        &mut uninitialized_sandbox,
        tx.clone(),
        http_client,
        &agent_id,
        vm_manager,
    )?;

    // Initialize the sandbox
    let mut sandbox = uninitialized_sandbox.evolve(Noop::default())?;

    let name = match sandbox
        .call_guest_function_by_name(
            constants::GuestMethod::GetName.as_ref(),
            ReturnType::String,
            Some(vec![ParameterValue::String("".to_string())]),
        )
        .unwrap()
    {
        ReturnValue::String(s) => s,
        _ => panic!("Expected a string return value"),
    };

    let description = match sandbox
        .call_guest_function_by_name(
            constants::GuestMethod::GetDescription.as_ref(),
            ReturnType::String,
            Some(vec![ParameterValue::String("".to_string())]),
        )
        .unwrap()
    {
        ReturnValue::String(s) => s,
        _ => panic!("Expected a string return value"),
    };

    let params_str = match sandbox
        .call_guest_function_by_name(
            constants::GuestMethod::GetParams.as_ref(),
            ReturnType::String,
            Some(vec![ParameterValue::String("".to_string())]),
        )
        .unwrap()
    {
        ReturnValue::String(s) => s,
        _ => panic!("Expected a string return value"),
    };

    // Parse the JSON string to extract Param objects
    let mut params: Vec<Param> = Vec::new();

    // Remove the outer brackets and parse the JSON array
    let json_str = params_str.trim_start_matches('[').trim_end_matches(']');

    // Handle empty array case
    if !json_str.trim().is_empty() {
        // Split by commas that are not within objects
        let mut depth = 0;
        let mut start = 0;

        for (i, c) in json_str.chars().enumerate() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                ',' if depth == 0 => {
                    if let Some(param_json) = json_str[start..i]
                        .trim()
                        .strip_prefix('{')
                        .and_then(|s| s.strip_suffix('}'))
                    {
                        // Parse each parameter
                        let name: Vec<u8> = param_json
                            .split("\"name\": \"")
                            .nth(1)
                            .and_then(|s| s.split("\"").next())
                            .unwrap_or_default()
                            .to_string()
                            .into();

                        let required = param_json.contains("\"required\": true");

                        // Default to String type since it's not included in the serialized format
                        params.push(Param {
                            name,
                            description: None,
                            param_type: hyperlight_agents_common::traits::agent::ParamType::String,
                            required,
                        });
                    }
                    start = i + 1;
                }
                _ => {}
            }
        }

        // Handle the last parameter
        if start < json_str.len() {
            if let Some(param_json) = json_str[start..]
                .trim()
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
            {
                let name: Vec<u8> = param_json
                    .split("\"name\": \"")
                    .nth(1)
                    .and_then(|s| s.split("\"").next())
                    .unwrap_or_default()
                    .to_string()
                    .into();

                let required = param_json.contains("\"required\": true");

                params.push(Param {
                    name: name,
                    description: None,
                    param_type: hyperlight_agents_common::traits::agent::ParamType::String,
                    required,
                });
                for param in &params {
                    println!("Added parameter: {:?}", param);
                }
            }
        }
    }

    Ok(Agent {
        id: agent_id.split("/").last().unwrap().to_string(),
        name,
        description,
        params,
        sandbox,
        tx,
        rx,
        request_id: None,
    })
}

pub fn register_host_functions(
    sandbox: &mut UninitializedSandbox,
    tx: Sender<(Option<String>, String)>,
    http_client: Arc<Client>,
    agent_id: &str,
    vm_manager: Arc<VmManager>,
) -> hyperlight_host::Result<()> {
    // HTTP GET function
    let tx_clone = tx.clone();
    let http_get_fn = Arc::new(Mutex::new(move |url: String, callback_name: String| {
        let client = http_client.clone();
        let sender = tx_clone.clone();

        // Create a new thread with a runtime for the HTTP request
        std::thread::spawn(move || {
            // Create a runtime for this thread
            let rt = tokio::runtime::Runtime::new().unwrap();
            let response = rt.block_on(async {
                match http_request(client, &url, "GET", None, None).await {
                    Ok(resp) => resp,
                    Err(e) => format!("HTTP request failed: {}", e),
                }
            });

            if let Err(e) = sender.send((Some(response), callback_name)) {
                eprintln!("Failed to send response: {:?}", e);
            }
        });

        Ok("Http Request sent".to_string())
    }));

    let all_syscalls: Vec<i64> = (0..=500).collect();
    http_get_fn.register_with_extra_allowed_syscalls(
        sandbox,
        constants::HostMethod::FetchData.as_ref(),
        all_syscalls,
    )?;

    // Final answer function
    let agent_id_clone = agent_id.split("/").last().unwrap_or(agent_id).to_string();
    let print_final_answer_fn = Arc::new(Mutex::new(move |answer: String, _param: String| {
        println!("Agent {}: Final answer: {}", agent_id_clone, answer);

        // Capture a copy of the answer for the response
        let answer_copy = answer.clone();

        // Instead of a separate thread, directly process the response here
        // to avoid potential race conditions with the response channel
        println!("Finalresult called for agent {}", agent_id_clone);

        // Look up the request ID for this agent
        let request_id = {
            if let Ok(request_ids) = MCP_AGENT_REQUEST_IDS.lock() {
                println!("DEBUG: FULL request map: {:?}", *request_ids);
                println!(
                    "Available agent IDs in request map: {:?}",
                    request_ids.keys().collect::<Vec<_>>()
                );

                if let Some(request_id) = request_ids.get(&agent_id_clone) {
                    Some(request_id.clone())
                } else {
                    println!(
                        "No active request ID found for agent {}. Map contains: {:?}",
                        agent_id_clone, request_ids
                    );
                    None
                }
            } else {
                eprintln!("Failed to lock request IDs");
                None
            }
        };

        // If we found a request ID, send the answer
        if let Some(request_id) = request_id {
            println!(
                "Found request ID {} for agent {}",
                request_id, agent_id_clone
            );

            // Send the answer back through the MCP response channel
            if let Ok(mut channels) = MCP_RESPONSE_CHANNELS.lock() {
                if let Some(tx) = channels.remove(&request_id) {
                    println!("Sending final result to MCP for request {}", request_id);

                    match tx.send(answer_copy) {
                        Ok(_) => {
                            println!(
                                "Successfully sent final result to MCP for request {}",
                                request_id
                            );
                            // Don't remove the request ID here - let the MCP server handle cleanup
                            // when it receives the response
                        }
                        Err(e) => {
                            eprintln!("Failed to send final result to MCP server: {}", e);
                        }
                    }
                } else {
                    eprintln!("No response channel found for request ID {}", request_id);
                }
            } else {
                eprintln!("Failed to lock response channels");
            }
        }

        // Return quickly to avoid lock contention with the guest
        Ok(())
    }));
    let all_syscalls: Vec<i64> = (0..=500).collect();
    print_final_answer_fn.register_with_extra_allowed_syscalls(
        sandbox,
        constants::HostMethod::FinalResult.as_ref(),
        all_syscalls,
    )?;

    // VM Management Functions
    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();
    let create_vm_fn = Arc::new(Mutex::new(move |vm_id: String, callback_name: String| {
        let vm_manager = vm_manager_clone.clone();
        let sender = tx_clone.clone();

        // Create a new thread with a runtime for the VM creation
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let response = rt.block_on(async {
                match vm_manager.create_vm(vm_id).await {
                    Ok(resp) => resp,
                    Err(e) => format!("VM creation failed: {}", e),
                }
            });

            if let Err(e) = sender.send((Some(response), callback_name)) {
                eprintln!("Failed to send VM creation response: {:?}", e);
            }
        });

        Ok("VM creation initiated".to_string())
    }));
    let all_syscalls: Vec<i64> = (0..=500).collect();
    create_vm_fn.register_with_extra_allowed_syscalls(
        sandbox,
        "create_vm",
        all_syscalls.clone(),
    )?;

    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();
    let execute_vm_command_fn = Arc::new(Mutex::new(move |vm_id: String, command_json: String| {
        let vm_manager = vm_manager_clone.clone();
        let sender = tx_clone.clone();

        // Parse command JSON
        let command_data: serde_json::Value = match serde_json::from_str(&command_json) {
            Ok(data) => data,
            Err(e) => {
                return Ok(format!("Failed to parse command JSON: {}", e));
            }
        };

        let command = command_data["command"].as_str().unwrap_or("").to_string();
        let args: Vec<String> = command_data["args"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let working_dir = command_data["working_dir"].as_str().map(|s| s.to_string());
        let timeout_seconds = command_data["timeout_seconds"].as_u64();

        // Create a new thread with a runtime for the VM command execution
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let response = rt.block_on(async {
                match vm_manager
                    .execute_command_in_vm(&vm_id, command, args, working_dir, timeout_seconds)
                    .await
                {
                    Ok(resp) => resp,
                    Err(e) => format!("VM command execution failed: {}", e),
                }
            });

            if let Err(e) = sender.send((Some(response), "vm_command_result".to_string())) {
                eprintln!("Failed to send VM command response: {:?}", e);
            }
        });

        Ok("VM command execution initiated".to_string())
    }));
    execute_vm_command_fn.register_with_extra_allowed_syscalls(
        sandbox,
        "execute_vm_command",
        all_syscalls.clone(),
    )?;

    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();
    let destroy_vm_fn = Arc::new(Mutex::new(move |vm_id: String, callback_name: String| {
        let vm_manager = vm_manager_clone.clone();
        let sender = tx_clone.clone();

        // Create a new thread with a runtime for the VM destruction
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let response = rt.block_on(async {
                match vm_manager.destroy_vm(&vm_id).await {
                    Ok(resp) => resp,
                    Err(e) => format!("VM destruction failed: {}", e),
                }
            });

            if let Err(e) = sender.send((Some(response), callback_name)) {
                eprintln!("Failed to send VM destruction response: {:?}", e);
            }
        });

        Ok("VM destruction initiated".to_string())
    }));
    destroy_vm_fn.register_with_extra_allowed_syscalls(
        sandbox,
        "destroy_vm",
        all_syscalls.clone(),
    )?;

    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();
    let list_vms_fn = Arc::new(Mutex::new(move |_param1: String, callback_name: String| {
        let vm_manager = vm_manager_clone.clone();
        let sender = tx_clone.clone();

        // Create a new thread for the VM listing
        std::thread::spawn(move || {
            let vms = vm_manager.list_vms();
            let response = serde_json::to_string(&vms).unwrap_or_else(|_| "[]".to_string());

            if let Err(e) = sender.send((Some(response), callback_name)) {
                eprintln!("Failed to send VM list response: {:?}", e);
            }
        });

        Ok("VM list request initiated".to_string())
    }));
    list_vms_fn.register_with_extra_allowed_syscalls(sandbox, "list_vms", all_syscalls.clone())?;

    Ok(())
}

pub fn run_agent_event_loop(agent: &mut Agent) {
    loop {
        match agent.rx.try_recv() {
            Ok((content, callback_name)) => {
                // Store the request ID if it's included in the message
                if let Some(content_str) = &content {
                    if content_str.starts_with("mcp_request:") {
                        let parts: Vec<&str> = content_str.splitn(3, ':').collect();
                        if parts.len() >= 3 {
                            let request_id = parts[1].to_string();
                            agent.request_id = Some(request_id.clone());

                            // Store the request ID in the global map for the finalresult function to use
                            if let Ok(mut request_ids) =
                                crate::mcp_server::MCP_AGENT_REQUEST_IDS.lock()
                            {
                                println!(
                                    "Storing request ID {} for agent {}",
                                    request_id, agent.id
                                );
                                println!(
                                    "DEBUG: Before insertion - request map: {:?}",
                                    *request_ids
                                );
                                request_ids.insert(agent.id.clone(), request_id);
                                println!(
                                    "DEBUG: After insertion - request map: {:?}",
                                    *request_ids
                                );
                                println!(
                                    "Global agent request IDs map now contains: {:?}",
                                    request_ids
                                );
                            }

                            // Extract the actual message content
                            let actual_content = parts[2].to_string();

                            let callback_result = agent.sandbox.call_guest_function_by_name(
                                &callback_name,
                                ReturnType::String,
                                Some(vec![ParameterValue::String(actual_content)]),
                            );

                            // Don't automatically send the result back to MCP - wait for finalresult call
                            handle_callback_result(agent, callback_result);
                            continue;
                        }
                    }
                }

                // Regular callback handling (non-MCP messages)
                let callback_result = match content {
                    Some(content) => agent.sandbox.call_guest_function_by_name(
                        &callback_name,
                        ReturnType::String,
                        Some(vec![ParameterValue::String(content)]),
                    ),
                    None => agent.sandbox.call_guest_function_by_name(
                        &callback_name,
                        ReturnType::String,
                        Some(vec![]),
                    ),
                };

                handle_callback_result(agent, callback_result);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // No responses yet
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                println!("Agent {} channel disconnected", agent.id);

                // Clean up any request IDs when the agent disconnects
                if let Ok(mut request_ids) = MCP_AGENT_REQUEST_IDS.lock() {
                    request_ids.remove(&agent.id);
                }

                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn handle_callback_result(
    agent: &mut Agent,
    callback_result: Result<ReturnValue, hyperlight_host::HyperlightError>,
) {
    match callback_result {
        Ok(result) => {
            println!("Agent {} callback returned: {:?}", agent.id, result);

            // Do not automatically send results back to MCP
            // The finalresult host function will handle that
            // When the agent calls the finalresult host function, it will use the request_id to send back the result
        }
        Err(e) => {
            eprintln!("Agent {} callback error: {:?}", agent.id, e);

            // Send error back to MCP server if there's an active request
            if let Some(request_id) = &agent.request_id {
                let error_msg = format!("Error: {:?}", e);
                if let Ok(mut channels) = MCP_RESPONSE_CHANNELS.lock() {
                    if let Some(tx) = channels.remove(request_id) {
                        if let Err(e) = tx.send(error_msg) {
                            eprintln!("Failed to send error response to MCP server: {}", e);
                        }
                    }
                }

                // Remove the request ID from the global map
                if let Ok(mut request_ids) = crate::mcp_server::MCP_AGENT_REQUEST_IDS.lock() {
                    request_ids.remove(&agent.id);
                }

                // Clear the local request ID
                agent.request_id = None;
            }
        }
    }
}
