use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hyperlight_host::sandbox::SandboxConfiguration;
use hyperlight_host::sandbox_state::sandbox::EvolvableSandbox;
use hyperlight_host::sandbox_state::transition::Noop;
use hyperlight_host::{MultiUseSandbox, UninitializedSandbox};
//use opentelemetry::global::{self};
//use opentelemetry::trace::{Span, TraceContextExt, Tracer};
//use opentelemetry::Context;

use crate::host_functions::network_functions::http_request;
use crate::host_functions::vm_functions::VmManager;
use crate::mcp_server::{MCP_AGENT_REQUEST_IDS, MCP_RESPONSE_CHANNELS};
use hyperlight_agents_common::{constants, Tool};
use reqwest::Client;
use std::sync::mpsc::{channel, Receiver, Sender};

pub struct Agent {
    pub id: String,
    pub name: String,
    pub mcp_tool: Tool,
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

    // Create a more permissive sandbox configuration
    let mut sandbox_config = SandboxConfiguration::default();
    sandbox_config.set_input_data_size(100 * 1024 * 1024);
    sandbox_config.set_output_data_size(100 * 1024 * 1024);
    sandbox_config.set_heap_size(100 * 1024 * 1024);

    let mut uninitialized_sandbox =
        UninitializedSandbox::new(guest_instance, Some(sandbox_config))?;

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

    let mcp_tool = sandbox
        .call_guest_function_by_name::<String>(constants::GuestMethod::GetMCPTool.as_ref(), ())
        .unwrap();

    let mcp_tool_deserialized: Tool = serde_json::from_str(&mcp_tool)?;

    Ok(Agent {
        id: agent_id.split("/").last().unwrap().to_string(),
        name: mcp_tool_deserialized.name.clone(),
        mcp_tool: mcp_tool_deserialized,
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

            // let tracer = global::tracer("host_method");
            // let span = tracer.start("HostMethod::FetchData");
            // let cx = Context::current_with_span(span);

            std::thread::spawn(move || {
                //let tracer = global::tracer("host_method");
                //let mut child_span = tracer.start_with_context("http_request", &cx);

                let rt = tokio::runtime::Runtime::new().unwrap();
                let response = rt.block_on(async {
                    match http_request(client, &url, "GET", None, None).await {
                        Ok(resp) => {
                            //child_span.add_event(format!("Http Request {}", &url), vec![]);
                            resp
                        }
                        Err(e) => format!("HTTP request failed: {}", e),
                    }
                });

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    log::error!("Failed to send response: {:?}", e);
                }

                //child_span.end();
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
            log::debug!("Finalresult called for agent {}", agent_id_clone);

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
                    log::error!("Failed to send VM creation response: {:?}", e);
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
                        .execute_vm_command(
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
                    log::error!("Failed to send VM command response: {:?}", e);
                }
            });

            Ok("VM command execution initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    // Register SpawnVMProcess host method
    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::SpawnCommand.as_ref(),
        move |vm_id: String, process_args: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let response = rt.block_on(async {
                    match vm_manager.spawn_command(&vm_id, process_args).await {
                        Ok(resp) => resp,
                        Err(e) => format!("VM process spawn failed: {}", e),
                    }
                });

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    log::error!("Failed to send VM process spawn response: {:?}", e);
                }
            });

            Ok("VM process spawn initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    // Register ListSpawnedProcesses host method
    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::ListSpawnedProcesses.as_ref(),
        move |vm_id: String, callback_name: String| {
            log::debug!("List spawned processes initiated for vm {}", vm_id);
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let response = rt.block_on(async {
                    match vm_manager.list_spawned_processes(&vm_id).await {
                        Ok(list) => {
                            serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
                        }
                        Err(e) => format!("List spawned processes failed: {}", e),
                    }
                });

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    log::error!("Failed to send list spawned processes response: {:?}", e);
                }
            });

            Ok("List spawned processes initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    // Register SpawnCommand host method
    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::SpawnCommand.as_ref(),
        move |vm_id: String, command_args: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let response = rt.block_on(async {
                    match vm_manager.spawn_command(&vm_id, command_args).await {
                        Ok(resp) => resp,
                        Err(e) => format!("Spawn command failed: {}", e),
                    }
                });

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    log::error!("Failed to send spawn command response: {:?}", e);
                }
            });

            Ok("Spawn command initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    // Register ListSpawnedProcesses host method
    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::ListSpawnedProcesses.as_ref(),
        move |vm_id: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let response = rt.block_on(async {
                    match vm_manager.list_spawned_processes(&vm_id).await {
                        Ok(list) => {
                            serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string())
                        }
                        Err(e) => format!("List spawned processes failed: {}", e),
                    }
                });

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    log::error!("Failed to send list spawned processes response: {:?}", e);
                }
            });

            Ok("List spawned processes initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    // Register StopSpawnedProcess host method
    let vm_manager_clone = vm_manager.clone();
    let tx_clone = tx.clone();

    sandbox.register_with_extra_allowed_syscalls(
        constants::HostMethod::StopSpawnedProcess.as_ref(),
        move |vm_id: String, process_id: String, callback_name: String| {
            let vm_manager = vm_manager_clone.clone();
            let sender = tx_clone.clone();

            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let response = rt.block_on(async {
                    match vm_manager.stop_spawned_process(&vm_id, &process_id).await {
                        Ok(resp) => resp,
                        Err(e) => format!("Stop spawned process failed: {}", e),
                    }
                });

                if let Err(e) = sender.send((Some(response), callback_name)) {
                    log::error!("Failed to send stop spawned process response: {:?}", e);
                }
            });

            Ok("Stop spawned process initiated".to_string())
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
                    log::error!("Failed to send VM destruction response: {:?}", e);
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
                    log::error!("Failed to send VM list response: {:?}", e);
                }
            });

            Ok("VM list request initiated".to_string())
        },
        all_syscalls.clone(),
    )?;

    Ok(())
}

pub fn run_agent_event_loop(agent: &mut Agent, shutdown_flag: Arc<AtomicBool>) {
    log::debug!("Agent {} event loop started", agent.id);

    loop {
        // Check for shutdown signal first
        if shutdown_flag.load(Ordering::Relaxed) {
            log::debug!(
                "Agent {} received shutdown signal, exiting event loop",
                agent.id
            );
            break;
        }

        match agent.rx.try_recv() {
            Ok((content, callback_name)) => {
                // Check shutdown flag again before processing message
                if shutdown_flag.load(Ordering::Relaxed) {
                    log::debug!(
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
                                log::trace!(
                                    "Storing request ID {} for agent {}",
                                    request_id,
                                    agent.id
                                );

                                request_ids.insert(agent.id.clone(), request_id);
                            }

                            // Extract the actual message content
                            let actual_content = parts[2].to_string();

                            log::trace!(
                                "Callback function called: {}, params: {:?}",
                                callback_name,
                                actual_content
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
                                log::trace!("Agent {} received shutdown signal after processing message, exiting", agent.id);
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
                    log::trace!(
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
                log::warn!("Agent {} channel disconnected", agent.id);

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

    log::debug!("Agent {} event loop terminated", agent.id);
}

fn handle_callback_result(
    agent: &mut Agent,
    callback_result: Result<String, hyperlight_host::HyperlightError>,
) {
    match callback_result {
        Ok(result) => {
            log::debug!("Agent {} callback returned: {:?}", agent.id, result);

            // Do not automatically send results back to MCP
            // The finalresult host function will handle that
            // When the agent calls the finalresult host function, it will use the request_id to send back the result
        }
        Err(e) => {
            log::error!("Agent {} callback error: {:?}", agent.id, e);

            // Send error back to MCP server if there's an active request
            if let Some(request_id) = &agent.request_id {
                let error_msg = format!("Error: {:?}", e);
                if let Ok(mut channels) = MCP_RESPONSE_CHANNELS.lock() {
                    if let Some(tx) = channels.remove(request_id) {
                        if let Err(e) = tx.send(error_msg) {
                            log::error!("Failed to send error response to MCP server: {}", e);
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
