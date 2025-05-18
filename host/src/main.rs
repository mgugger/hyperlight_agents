use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

mod agents;
mod host_functions;
mod mcp_server;

fn main() -> hyperlight_host::Result<()> {
    // Create the MCP server
    let mcp_server = mcp_server::McpServer::new();

    // Initialize the MCP agent metadata global state
    let agent_metadata: Arc<Mutex<HashMap<String, (String, String)>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let http_client = Arc::new(
        reqwest::blocking::ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
    );

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
        )?;
        agents.push(agent);
    }

    // senders
    let mut tx_senders = Vec::new();
    for agent in &agents {
        tx_senders.push((agent.id.clone(), agent.tx.clone()));
        // Register the agent with the MCP server with metadata
        mcp_server.register_agent(
            agent.id.clone(),
            agent.name.clone(),
            agent.description.clone(),
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

    // Start the MCP server on port 3000
    let server_handle = mcp_server.start_server(SocketAddr::from(([127, 0, 0, 1], 3000)));

    println!("\n=================================================");
    println!("MCP Server started at http://127.0.0.1:3000");
    println!("Agents registered: {}", tx_senders.len());
    println!("\nAPI Endpoints:");
    println!("  GET  /agents - List all available agents");
    println!("  GET  /tools  - Get tools in OpenAI format");
    println!("  POST /       - Send message to an agent");
    println!("\nGitHub Copilot Integration:");
    println!("  /lsp or /copilot - LSP endpoint for GitHub Copilot");
    println!("  Add server as a Remote Tool in VS Code settings");
    println!("=================================================\n");

    // Optional: directly send a message to the first agent, as in the original code
    // if let Some((id, tx)) = tx_senders.first() {
    //     tx.send((None, "TopHNLinks".to_string()))
    //         .expect(&format!("Failed to send message to agent {}", id));
    // }

    // Wait for all agents to complete
    for handle in handles {
        let _ = handle.join();
    }

    // Wait for the server to complete (will never happen in this implementation)
    let _ = server_handle.join();

    Ok(())
}
