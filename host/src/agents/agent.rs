use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hyperlight_host::sandbox::{ExtraAllowedSyscall, SandboxConfiguration};
use hyperlight_host::sandbox_state::sandbox::EvolvableSandbox;
use hyperlight_host::sandbox_state::transition::Noop;
use hyperlight_host::{MultiUseSandbox, UninitializedSandbox};

use crate::host_functions::firecracker_vm_functions::VmManager;
use crate::host_functions::network_functions::http_request;
use crate::mcp_server::{MCP_AGENT_REQUEST_IDS, MCP_RESPONSE_CHANNELS};
use hyperlight_agents_common::constants;
use reqwest::Client;
use std::sync::mpsc::{channel, Receiver, Sender};

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

    // Create a sandbox for this agent
    let guest_instance = hyperlight_host::GuestBinary::FilePath(binary_path);

    println!("DEBUG: Starting agent creation for {}", agent_id);

    println!("DEBUG: Creating UninitializedSandbox with custom config...");

    // Create a more permissive sandbox configuration
    let mut sandbox_config = SandboxConfiguration::default();
    sandbox_config.set_input_data_size(100 * 1024 * 1024);
    sandbox_config.set_output_data_size(100 * 1024 * 1024);
    sandbox_config.set_heap_size(100 * 1024 * 1024);

    let mut uninitialized_sandbox =
        UninitializedSandbox::new(guest_instance, Some(sandbox_config))?;

    println!("DEBUG: Registering host functions...");
    // Register host functions specific to this agent
    register_host_functions(
        &mut uninitialized_sandbox,
        tx.clone(),
        http_client,
        &agent_id,
        vm_manager,
    )?;

    // Initialize the sandbox
    println!("DEBUG: Evolving sandbox...");
    let mut sandbox = uninitialized_sandbox.evolve(Noop::default())?;

    println!("DEBUG: Calling guest GetName function...");
    let name = sandbox
        .call_guest_function_by_name::<String>(constants::GuestMethod::GetName.as_ref(), ())
        .unwrap();

    println!("DEBUG: Calling guest GetDescription function...");
    let description = sandbox
        .call_guest_function_by_name::<String>(constants::GuestMethod::GetDescription.as_ref(), ())
        .unwrap();

    println!("DEBUG: Calling guest GetParams function...");
    let params_str = sandbox
        .call_guest_function_by_name::<String>(constants::GuestMethod::GetParams.as_ref(), ())
        .unwrap();

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
                        let name: String = param_json
                            .split("\"name\": \"")
                            .nth(1)
                            .and_then(|s| s.split("\"").next())
                            .unwrap_or_default()
                            .to_string()
                            .into();

                        let description: String = param_json
                            .split("\"description\": \"")
                            .nth(1)
                            .and_then(|s| s.split("\"").next())
                            .unwrap_or_default()
                            .to_string()
                            .into();

                        let required = param_json.contains("\"required\": true");

                        // Default to String type since it's not included in the serialized format
                        params.push(Param {
                            name,
                            description: Some(description),
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
                let name: String = param_json
                    .split("\"name\": \"")
                    .nth(1)
                    .and_then(|s| s.split("\"").next())
                    .unwrap_or_default()
                    .to_string()
                    .into();

                let description: String = param_json
                    .split("\"description\": \"")
                    .nth(1)
                    .and_then(|s| s.split("\"").next())
                    .unwrap_or_default()
                    .to_string()
                    .into();

                let required = param_json.contains("\"required\": true");

                params.push(Param {
                    name: name,
                    description: Some(description),
                    param_type: hyperlight_agents_common::traits::agent::ParamType::String,
                    required,
                });
                // for param in &params {
                //     println!("Added parameter: {:?}", param);
                // }
            }
        }
    }

    println!("DEBUG: Agent creation completed successfully");
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
    // Define common syscalls that guest code might need
    let all_syscalls: Vec<i64> = (0..=500).collect();

    // Register HTTP fetch function with extra allowed syscalls
    let http_client_clone = http_client.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::FetchData.as_ref(),
        move |url: String, callback_name: String| {
            let client = http_client_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
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
        },
        all_syscalls.clone(),
    )?;

    // Register final result function
    let agent_id_clone = agent_id.split("/").last().unwrap_or(agent_id).to_string();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::FinalResult.as_ref(),
        move |answer: String, _param: String| {
            println!("Finalresult called for agent {}", agent_id_clone);

            // Look up the request ID for this agent
            let request_id = {
                if let Ok(request_ids) = MCP_AGENT_REQUEST_IDS.lock() {
                    request_ids.get(&agent_id_clone).cloned()
                } else {
                    None
                }
            };

            // If we found a request ID, send the answer
            if let Some(request_id) = request_id {
                if let Ok(mut channels) = MCP_RESPONSE_CHANNELS.lock() {
                    if let Some(tx) = channels.remove(&request_id) {
                        let _ = tx.send(answer);
                    }
                }
            }

            Ok(())
        },
        all_syscalls.clone(),
    )?;

    // Register VM management functions
    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::CreateVM.as_ref(),
        move |vm_id: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

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
        },
        all_syscalls.clone(),
    )?;

    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::ExecuteVMCommand.as_ref(),
        move |vm_id: String, command: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let response = rt.block_on(async {
                    match vm_manager
                        .execute_command_in_vm(
                            &vm_id,
                            command,
                            Vec::new(),
                            Some("/".to_string()),
                            Some(30),
                        )
                        .await
                    {
                        Ok(resp) => resp,
                        Err(e) => format!("VM command execution failed: {}", e),
                    }
                });

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    eprintln!("Failed to send VM command response: {:?}", e);
                }
            });

            Ok("VM command execution initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::DestroyVM.as_ref(),
        move |vm_id: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

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
        },
        all_syscalls.clone(),
    )?;

    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::ListVMs.as_ref(),
        move |_param1: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
                let vms = vm_manager.list_vms();
                let response = serde_json::to_string(&vms).unwrap_or_else(|_| "[]".to_string());

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    eprintln!("Failed to send VM list response: {:?}", e);
                }
            });

            Ok("VM list request initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    Ok(())
}

pub fn run_agent_event_loop(agent: &mut Agent, shutdown_flag: Arc<AtomicBool>) {
    println!("Agent {} event loop started", agent.id);

    loop {
        // Check for shutdown signal first
        if shutdown_flag.load(Ordering::Relaxed) {
            println!(
                "Agent {} received shutdown signal, exiting event loop",
                agent.id
            );
            break;
        }

        match agent.rx.try_recv() {
            Ok((content, callback_name)) => {
                // Check shutdown flag again before processing message
                if shutdown_flag.load(Ordering::Relaxed) {
                    println!(
                        "Agent {} received shutdown signal during message processing, exiting",
                        agent.id
                    );
                    break;
                }

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

                            println!(
                                "Callback function called: {}, params: {:?}",
                                callback_name, actual_content
                            );
                            let callback_result =
                                agent.sandbox.call_guest_function_by_name::<String>(
                                    &callback_name,
                                    actual_content,
                                );

                            // Don't automatically send the result back to MCP - wait for finalresult call
                            handle_callback_result(agent, callback_result);

                            // Check shutdown flag after processing
                            if shutdown_flag.load(Ordering::Relaxed) {
                                println!("Agent {} received shutdown signal after processing message, exiting", agent.id);
                                break;
                            }
                            continue;
                        }
                    }
                }

                // Regular callback handling (non-MCP messages)
                let callback_result = match content {
                    Some(content) => agent
                        .sandbox
                        .call_guest_function_by_name::<String>(&callback_name, content),
                    None => agent
                        .sandbox
                        .call_guest_function_by_name::<String>(&callback_name, ()),
                };

                handle_callback_result(agent, callback_result);

                // Check shutdown flag after processing
                if shutdown_flag.load(Ordering::Relaxed) {
                    println!(
                        "Agent {} received shutdown signal after processing callback, exiting",
                        agent.id
                    );
                    break;
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // No responses yet - this is where we sleep and check again
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

        // Sleep for a shorter duration for more responsive shutdown
        std::thread::sleep(Duration::from_millis(50));
    }

    println!("Agent {} event loop terminated", agent.id);
}

fn handle_callback_result(
    agent: &mut Agent,
    callback_result: Result<String, hyperlight_host::HyperlightError>,
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
