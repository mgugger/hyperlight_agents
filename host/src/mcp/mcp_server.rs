use hyperlight_agents_common::{traits::agent::Param, Tool};
use rust_mcp_schema::{
    Implementation, InitializeResult, ServerCapabilities, ServerCapabilitiesTools,
    LATEST_PROTOCOL_VERSION,
};
use rust_mcp_sdk::mcp_server::{
    hyper_server::{self},
    HyperServerOptions,
};
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

use crate::mcp::mcp_handler::HyperlightAgentHandler;

// Global response channels and agent metadata
lazy_static::lazy_static! {
    pub static ref MCP_RESPONSE_CHANNELS: Mutex<HashMap<String, oneshot::Sender<String>>> = Mutex::new(HashMap::new());
    pub static ref MCP_AGENT_METADATA: Mutex<HashMap<String, Tool>> = Mutex::new(HashMap::new());
    pub static ref MCP_AGENT_REQUEST_IDS: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
}

// Agent info structure for agents
#[derive(Serialize, Debug)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

// MCP server wrapper that manages agent channels
pub struct McpServerManager {
    pub agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
    agent_metadata: Arc<Mutex<HashMap<String, (String, String)>>>, // id -> (name, description)
}

impl McpServerManager {
    pub fn new() -> Self {
        McpServerManager {
            agent_channels: Arc::new(Mutex::new(HashMap::new())),
            agent_metadata: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register_agent(
        &self,
        agent_id: String,
        mcp_tool: Tool,
        tx: Sender<(Option<String>, String)>,
    ) {
        // Register the agent's channel
        let mut channels = self.agent_channels.lock().unwrap();
        channels.insert(agent_id.clone(), tx);
        log::debug!("Registered agent channel for '{}'", agent_id);

        // Register the agent's metadata in both local and global state
        let mut metadata = self.agent_metadata.lock().unwrap();
        metadata.insert(agent_id.clone(), ("".to_string(), "".to_string()));
        log::debug!("Registered agent metadata for '{}'", agent_id,);

        // Update global metadata
        if let Ok(mut global_metadata) = MCP_AGENT_METADATA.lock() {
            global_metadata.insert(agent_id.clone(), (mcp_tool));
            log::debug!("Updated MCP_AGENT_METADATA for '{}'", agent_id);
        }
    }

    pub async fn start_server(self, addr: SocketAddr) {
        let agent_channels = self.agent_channels.clone();

        log::debug!("Creating HyperlightAgentHandler with agent channels.");
        // Create a handler with agent channels
        let handler = HyperlightAgentHandler { agent_channels };

        log::debug!("Preparing MCP server configuration.");
        // Create server configuration
        let server_details = InitializeResult {
            // Server name and version
            server_info: Implementation {
                name: "Hyperlight Agents MCP Server".to_string(),
                version: "0.1.0".to_string(),
                title: Some("Hyperlight MCP Server".to_string()),
            },
            capabilities: ServerCapabilities {
                // Indicates that server supports MCP tools
                tools: Some(ServerCapabilitiesTools { list_changed: None }),
                ..Default::default() // Using default values for other fields
            },
            meta: None,
            instructions: Some("Use this server to interact with Hyperlight agents".to_string()),
            protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
        };

        let hyper_server_options = HyperServerOptions {
            host: addr.ip().to_string(),
            port: addr.port(),
            ..Default::default()
        };

        log::debug!("Creating Hyper server instance.");
        // Start the HTTP server with Hyper
        let server = hyper_server::create_server(server_details, handler, hyper_server_options);

        log::debug!("MCP server listening on http://{}", addr);
        log::debug!("MCP server about to start serving requests.");

        let result = server.start().await;
        match result {
            Ok(_) => {
                log::debug!("MCP server finished serving requests and exited normally.");
            }
            Err(e) => {
                log::error!("MCP server error: {:?}", e);
            }
        }
        log::debug!("MCP server start_server() function is returning.");
    }
}

// Log an MCP request with details
// fn log_mcp_request(tool_name: &str, message: &str, request_id: &str) {
//     let timestamp = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
//         Ok(n) => n.as_secs(),
//         Err(_) => 0,
//     };

//     println!("[{}] MCP REQUEST [ID: {}]", timestamp, request_id);
//     println!("  Tool: {}", tool_name);

//     // Log the message content
//     let preview_length = 1000;
//     let message_preview = if message.len() > preview_length {
//         format!(
//             "{}... [truncated {} bytes]",
//             &message[..preview_length],
//             message.len() - preview_length
//         )
//     } else {
//         message.to_string()
//     };
//     println!("  Message: {}", message_preview);
//     println!(""); // Add empty line for separation
// }
