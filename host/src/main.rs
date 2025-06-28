use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};

use host_functions::firecracker_vm_functions::VmManager;
use mcp::mcp_server;

mod agents;
mod host_functions;
mod mcp;

#[tokio::main]
async fn main() -> hyperlight_host::Result<()> {
    // Create the MCP server manager
    let mcp_server_manager = mcp_server::McpServerManager::new();

    // Initialize the MCP agent metadata global state
    // let agent_metadata: Arc<Mutex<HashMap<String, (String, String)>>> =
    //    Arc::new(Mutex::new(HashMap::new()));

    let http_client = Arc::new(
        reqwest::ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
    );

    // Create VM manager and start VSOCK server
    let vm_manager = Arc::new(VmManager::new());
    if let Err(e) = vm_manager.start_vsock_server(1234) {
        eprintln!("Failed to start VSOCK server: {}", e);
    } else {
        println!("VSOCK server started on port 1234");
    }

    let agent_ids: Vec<String> = std::fs::read_dir("./../target/x86_64-unknown-none/debug/")
        .or_else(|_| std::fs::read_dir("./target/x86_64-unknown-none/debug/"))
        .expect("Failed to read directory")
        .filter_map(|entry| {
            entry.ok().and_then(|e| {
                let path = e.path();
                if path.is_file()
                    && !path.to_string_lossy().ends_with(".d")
                    && !path.to_string_lossy().ends_with(".cargo-lock")
                {
                    println!("Found agent binary: {}", path.display());
                    Some(path.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
        })
        .collect();
    let mut agents = Vec::new();

    for agent_id in agent_ids {
        println!("Creating agent for: {}", agent_id);
        match agents::agent::create_agent(
            agent_id.to_string(),
            http_client.clone(),
            agent_id.to_string(),
            vm_manager.clone(),
        ) {
            Ok(agent) => {
                println!("✓ Agent created successfully: {}", agent.name);
                agents.push(agent);
            }
            Err(e) => {
                println!("✗ Failed to create agent {}: {:?}", agent_id, e);
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
            agent.name.clone(),
            agent.description.clone(),
            agent.params.clone(),
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

    println!("\n=================================================");
    println!("MCP Server starting at http://127.0.0.1:3000/sse");
    println!("Agents registered: {}", tx_senders.len());
    println!("Press Ctrl+C to shutdown gracefully");
    println!("=================================================\n");

    // Start the MCP server with the rust-mcp-sdk (now async)
    // Create a clone of vm_manager for cleanup
    let vm_manager_cleanup = vm_manager.clone();
    
    // Create a cancellation token for the server
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    
    let server_handle = tokio::spawn(async move {
        // Run the server in a select with the shutdown signal
        tokio::select! {
            _ = mcp_server_manager.start_server(addr) => {
                println!("MCP server completed naturally");
            }
            _ = &mut shutdown_rx => {
                println!("MCP server received shutdown signal");
            }
        }
    });

    // Create an abort handle before the select
    let abort_handle = server_handle.abort_handle();

    // Wait for shutdown signal or server completion
    tokio::select! {
        result = server_handle => {
            match result {
                Ok(_) => println!("MCP server task completed successfully"),
                Err(e) => println!("MCP server task failed: {:?}", e),
            }
        }
        _ = tokio::signal::ctrl_c() => {
            println!("Received Ctrl+C, shutting down gracefully...");
            
            // Send shutdown signal to the server
            let _ = shutdown_tx.send(());
            
            // Give the server a moment to shut down gracefully
            tokio::time::sleep(Duration::from_millis(500)).await;
            
            // Abort the server task if it's still running
            abort_handle.abort();
            
            println!("MCP server shutdown initiated");
        }
    }

    // Perform cleanup
    println!("Shutting down VM Manager...");
    vm_manager_cleanup.shutdown();
    
    // Perform emergency cleanup as well
    VmManager::emergency_cleanup();

    // Signal all agent threads to shutdown
    println!("Signaling agent threads to shutdown...");
    shutdown_flag.store(true, Ordering::Relaxed);

    // Drop all tx_senders to disconnect agent channels (helps threads exit faster)
    println!("Dropping agent senders to disconnect channels...");
    drop(tx_senders);

    // Wait for all agents to complete (with timeout)
    println!("Waiting for agent threads to complete...");
    let mut completed = 0;
    for handle in handles {
        match handle.join() {
            Ok(_) => {
                completed += 1;
                println!("Agent thread completed (total: {})", completed);
            }
            Err(e) => eprintln!("Agent thread panicked: {:?}", e),
        }
    }
    println!("All agent threads completed: {}", completed);

    println!("Application shutdown complete");

    Ok(())
}
