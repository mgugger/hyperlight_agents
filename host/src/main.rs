use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

    let agent_ids: Vec<String> = std::fs::read_dir("./../guest/target/x86_64-unknown-none/debug/")
        .expect("Failed to read directory")
        .filter_map(|entry| {
            entry.ok().and_then(|e| {
                let path = e.path();
                if path.is_file()
                    && !path.to_string_lossy().ends_with(".d")
                    && !path.to_string_lossy().ends_with(".cargo-lock")
                {
                    Some(path.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
        })
        .collect();
    let mut agents = Vec::new();

    for agent_id in agent_ids {
        let agent = agents::agent::create_agent(
            agent_id.to_string(),
            http_client.clone(),
            agent_id.to_string(),
            vm_manager.clone(),
        )?;
        agents.push(agent);
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

    // Start agent tasks in separate threads
    let mut handles = Vec::new();
    for mut agent in agents {
        let handle = thread::spawn(move || {
            agents::agent::run_agent_event_loop(&mut agent);
        });
        handles.push(handle);
    }

    // Create the MCP server with HTTP and SSE support
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    println!("\n=================================================");
    println!("MCP Server starting at http://127.0.0.1:3000/sse");
    println!("Agents registered: {}", tx_senders.len());
    println!("=================================================\n");

    // Start the MCP server with the rust-mcp-sdk (now async)
    mcp_server_manager.start_server(addr).await;

    // Wait for all agents to complete
    for handle in handles {
        let _ = handle.join();
    }

    Ok(())
}
