use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use host_functions::vm_functions::VmManager;

use mcp::mcp_server;

mod agents;
mod host_functions;
mod host_logger;
mod mcp;

use log::{debug, error, info};

use opentelemetry::global::{self};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use reqwest::Client;

#[tokio::main]
async fn main() -> hyperlight_host::Result<()> {
    // Initialize unified host logger
    host_logger::init_logger();

    /*
    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_protocol(Protocol::Grpc)
        .build()
        .unwrap();

    let resource = Resource::builder()
        .with_attributes(vec![
            KeyValue::new("service.name", "hyperlight_agents"),
            KeyValue::new("service.namespace", "my-application-group"),
            KeyValue::new("deployment.environment", "production"),
        ])
        .build();

    // Create a tracer provider with the exporter
    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(otlp_exporter)
        .with_resource(resource)
        .build();

    // Set it as the global provider
    global::set_tracer_provider(tracer_provider);
    */

    // Create the MCP server manager
    let mcp_server_manager = mcp_server::McpServerManager::new();

    let reqwest_client: reqwest::Client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    let http_client = Arc::new(reqwest_client);

    // Create VM manager and start VSOCK servers
    let vm_manager = Arc::new(VmManager::new());
    if let Err(e) = vm_manager.start_vsock_server(1234) {
        error!("Failed to start VSOCK server: {}", e);
    } else {
        debug!("VSOCK server started on port 1234");
    }

    // Start HTTP proxy VSOCK server
    if let Err(e) = vm_manager.start_http_proxy_server(1235) {
        error!("Failed to start HTTP proxy VSOCK server: {}", e);
    } else {
        debug!("HTTP proxy VSOCK server started on port 1235");
    }

    // Start log listener VSOCK server
    if let Err(e) = vm_manager.start_log_listener_server(1236) {
        error!("Failed to start HTTP proxy VSOCK server: {}", e);
    } else {
        debug!("HTTP proxy VSOCK server started on port 1236");
    }

    let agent_ids: Vec<String> = std::fs::read_dir("./guest/target/x86_64-unknown-none/debug/")
        .or_else(|_| std::fs::read_dir("./guest/target/x86_64-unknown-none/release/"))
        .expect("Failed to read directory")
        .filter_map(|entry| {
            entry.ok().and_then(|e| {
                let path = e.path();
                if path.is_file()
                    && !path.to_string_lossy().ends_with(".d")
                    && !path.to_string_lossy().ends_with(".cargo-lock")
                {
                    debug!("Found agent binary: {}", path.display());
                    Some(path.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
        })
        .collect();
    let mut agents = Vec::new();

    for agent_id in agent_ids {
        debug!("Creating agent for: {}", agent_id);
        match agents::agent::create_agent(
            agent_id.to_string(),
            http_client.clone(),
            agent_id.to_string(),
            vm_manager.clone(),
        ) {
            Ok(agent) => {
                debug!("✓ Agent created successfully: {}", agent.mcp_tool.name);
                agents.push(agent);
            }
            Err(e) => {
                error!("✗ Failed to create agent {}: {:?}", agent_id, e);
                return Err(e);
            }
        }
    }

    // senders
    let mut tx_senders = Vec::new();
    for agent in &agents {
        tx_senders.push((agent.id.clone(), agent.tx.clone()));
        // Register the agent with the MCP server manager with metadata
        mcp_server_manager.register_agent(
            agent.id.clone(),
            agent.mcp_tool.clone(),
            agent.tx.clone(),
        );
    }

    // Create a global shutdown flag
    let shutdown_flag = Arc::new(AtomicBool::new(false));

    // Start agent tasks in separate threads
    let mut handles = Vec::new();
    for mut agent in agents {
        let shutdown_flag_clone = shutdown_flag.clone();
        let handle = thread::spawn(move || {
            agents::agent::run_agent_event_loop(&mut agent, shutdown_flag_clone);
        });
        handles.push(handle);
    }

    // Create the MCP server with HTTP and SSE support
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    debug!("\n=================================================");
    debug!("MCP Server starting at http://127.0.0.1:3000/sse");
    info!("Agents registered: {}", tx_senders.len());
    info!("Press Ctrl+C to shutdown");
    info!("=================================================\n");

    // Start the MCP server with the rust-mcp-sdk (now async)
    // Create a clone of vm_manager for cleanup
    let vm_manager_cleanup = vm_manager.clone();

    // Create a cancellation token for the server
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let server_handle = tokio::spawn(async move {
        // Run the server in a select with the shutdown signal
        tokio::select! {
            _ = mcp_server_manager.start_server(addr) => {
                debug!("MCP server completed naturally");
            }
            _ = &mut shutdown_rx => {
                debug!("MCP server received shutdown signal");
            }
        }
    });

    // Create an abort handle before the select
    let abort_handle = server_handle.abort_handle();

    // Wait for shutdown signal or server completion
    tokio::select! {
        result = server_handle => {
            match result {
                Ok(_) => info!("MCP server task completed successfully"),
                Err(e) => error!("MCP server task failed: {:?}", e),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C signal. Initiating graceful shutdown...");

            // Send shutdown signal to the server
            let _ = shutdown_tx.send(());

            // Give the server a moment to shut down gracefully
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Abort the server task if it's still running
            abort_handle.abort();

            info!("MCP server shutdown initiated. Waiting for server task to abort...");
        }
    }

    // Perform cleanup
    info!("Shutting down VM Manager... Ensuring all VMs are terminated.");
    vm_manager_cleanup.shutdown();

    // Perform emergency cleanup as well
    VmManager::emergency_cleanup();

    // Signal all agent threads to shutdown
    info!("Signaling agent threads to shutdown... Setting shutdown flag.");
    shutdown_flag.store(true, Ordering::Relaxed);

    // Drop all tx_senders to disconnect agent channels (helps threads exit faster)
    debug!("Dropping agent senders to disconnect channels... This will help threads exit faster.");
    drop(tx_senders);

    // Wait for all agents to complete (with timeout)
    debug!("Waiting for agent threads to complete... This may take some time if threads are busy.");
    let mut completed = 0;
    for handle in handles {
        match handle.join() {
            Ok(_) => {
                completed += 1;
                debug!("Agent thread completed (total: {})", completed);
            }
            Err(e) => error!("Agent thread panicked: {:?}", e),
        }
    }
    info!("All agent threads completed: {}", completed);

    info!("Application shutdown complete. All resources have been cleaned up.");

    Ok(())
}
