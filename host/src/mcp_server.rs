use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{self, Value, json};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::runtime::Runtime;

// Global response channels and agent metadata
lazy_static::lazy_static! {
    pub static ref MCP_RESPONSE_CHANNELS: Mutex<HashMap<String, Sender<String>>> = Mutex::new(HashMap::new());
    pub static ref MCP_AGENT_METADATA: Mutex<HashMap<String, (String, String)>> = Mutex::new(HashMap::new());
    pub static ref MCP_AGENT_REQUEST_IDS: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
}

// MCP protocol message types
#[derive(Deserialize, Debug)]
pub struct McpRequest {
    recipient: String,
    message: String,
    function: Option<String>,
}

// LSP protocol message types
#[derive(Deserialize, Debug)]
pub struct LspRequest {
    jsonrpc: String,
    id: Value,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize, Debug)]
pub struct LspResponse {
    jsonrpc: String,
    id: Value,
    result: Value,
}

#[derive(Serialize, Debug)]
pub struct LspErrorResponse {
    jsonrpc: String,
    id: Value,
    error: LspError,
}

#[derive(Serialize, Debug)]
pub struct LspError {
    code: i32,
    message: String,
}

// Agent info structure for the list agents endpoint
#[derive(Serialize, Debug)]
pub struct AgentInfo {
    id: String,
    name: String,
    description: String,
}

#[derive(Serialize, Debug)]
pub struct McpResponse {
    status: String,
    data: Option<String>,
    error: Option<String>,
}

pub struct McpServer {
    agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
    agent_metadata: Arc<Mutex<HashMap<String, (String, String)>>>, // id -> (name, description)
}

impl McpServer {
    pub fn new() -> Self {
        McpServer {
            agent_channels: Arc::new(Mutex::new(HashMap::new())),
            agent_metadata: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register_agent(
        &self,
        agent_id: String,
        name: String,
        description: String,
        tx: Sender<(Option<String>, String)>,
    ) {
        // Register the agent's channel
        let mut channels = self.agent_channels.lock().unwrap();
        channels.insert(agent_id.clone(), tx);

        // Register the agent's metadata in both local and global state
        let mut metadata = self.agent_metadata.lock().unwrap();
        metadata.insert(agent_id.clone(), (name.clone(), description.clone()));

        // Update global metadata
        if let Ok(mut global_metadata) = MCP_AGENT_METADATA.lock() {
            global_metadata.insert(agent_id, (name, description));
        }
    }

    pub fn start_server(self, addr: SocketAddr) -> thread::JoinHandle<()> {
        let agent_channels = self.agent_channels.clone();
        //let agent_metadata = self.agent_metadata.clone();

        thread::spawn(move || {
            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                let service = make_service_fn(move |_| {
                    let agent_channels = agent_channels.clone();

                    async move {
                        Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
                            let agent_channels = agent_channels.clone();

                            async move { handle_request(req, agent_channels.clone()).await }
                        }))
                    }
                });

                let server = Server::bind(&addr).serve(service);
                println!("MCP server listening on http://{}", addr);

                if let Err(e) = server.await {
                    eprintln!("Server error: {}", e);
                }
            });
        })
    }
}

async fn handle_request(
    req: Request<Body>,
    agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
) -> Result<Response<Body>, Infallible> {
    // Handle GET request for listing agents
    if req.method() == hyper::Method::GET && req.uri().path() == "/list" {
        return handle_list_agents(agent_channels).await;
    }

    // Handle GET request for functions in OpenAI format for GitHub Copilot
    if req.method() == hyper::Method::GET && req.uri().path() == "/agents" {
        return handle_tools_list(agent_channels).await;
    }

    // Handle LSP protocol requests
    if req.uri().path() == "/lsp" || req.uri().path() == "/copilot" {
        return handle_lsp_request(req, agent_channels).await;
    }

    if req.method() != hyper::Method::POST {
        return Ok(Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Body::from(
                "Only GET /agents, GET /tools, POST, and LSP requests are supported",
            ))
            .unwrap());
    }

    // Read the request body
    let body_bytes = match hyper::body::to_bytes(req.into_body()).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("Failed to read request body"))
                .unwrap());
        }
    };

    // Parse the MCP request
    let mcp_request: McpRequest = match serde_json::from_slice(&body_bytes) {
        Ok(req) => req,
        Err(e) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(format!("Invalid MCP request: {}", e)))
                .unwrap());
        }
    };

    // Get the agent's channel
    let agent_tx = {
        let channels = agent_channels.lock().unwrap();
        match channels.get(&mcp_request.recipient) {
            Some(tx) => tx.clone(),
            None => {
                return Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from(format!(
                        "Agent '{}' not found",
                        mcp_request.recipient
                    )))
                    .unwrap());
            }
        }
    };

    // Create a channel for the response
    let (resp_tx, resp_rx) = std::sync::mpsc::channel::<String>();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    {
        let mut response_channels = MCP_RESPONSE_CHANNELS.lock().unwrap();
        response_channels.insert(request_id.clone(), resp_tx);
    }

    // Send message to the agent
    let function_name = mcp_request
        .function
        .unwrap_or_else(|| "default_handler".to_string());
    // Wrap the message with MCP protocol info
    let mcp_message = format!("mcp_request:{}:{}", request_id, mcp_request.message);
    if let Err(e) = agent_tx.send((Some(mcp_message), function_name)) {
        return Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!(
                "Failed to send message to agent: {}",
                e
            )))
            .unwrap());
    }

    println!(
        "Processing request for agent '{}' with ID: {}",
        mcp_request.recipient, request_id
    );

    // Wait for response with timeout - increased to 60 seconds to allow for finalresult
    let response = match wait_for_response(resp_rx, 120) {
        Some(resp) => McpResponse {
            status: "success".to_string(),
            data: Some(resp),
            error: None,
        },
        None => McpResponse {
            status: "error".to_string(),
            data: None,
            error: Some("Timeout waiting for agent response".to_string()),
        },
    };

    // Clean up the response channel and any request IDs
    {
        println!(
            "Request completed (ID: {}), cleaning up resources",
            request_id
        );
        let mut response_channels = MCP_RESPONSE_CHANNELS.lock().unwrap();
        response_channels.remove(&request_id);

        // Also make sure we remove any dangling request IDs for this request
        if let Ok(mut request_ids) = MCP_AGENT_REQUEST_IDS.lock() {
            let agents_to_clear: Vec<String> = request_ids
                .iter()
                .filter(|(_, req_id)| req_id == &&request_id)
                .map(|(agent_id, _)| agent_id.clone())
                .collect();

            for agent_id in agents_to_clear {
                request_ids.remove(&agent_id);
                println!("Cleaned up request ID mapping for agent: {}", agent_id);
            }
        }
    }

    // Return the response
    match serde_json::to_string(&response) {
        Ok(json) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json))
            .unwrap()),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!("Failed to serialize response: {}", e)))
            .unwrap()),
    }
}

// Handler for LSP protocol requests
async fn handle_lsp_request(
    req: Request<Body>,
    agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
) -> Result<Response<Body>, Infallible> {
    // Read the request body
    let body_bytes = match hyper::body::to_bytes(req.into_body()).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("Failed to read LSP request body"))
                .unwrap());
        }
    };

    // Parse as JSON-RPC request
    let json_value: Value = match serde_json::from_slice(&body_bytes) {
        Ok(val) => val,
        Err(e) => {
            println!("Failed to parse LSP request as JSON: {}", e);
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(format!("Invalid LSP request: {}", e)))
                .unwrap());
        }
    };

    println!(
        "Received LSP request: {}",
        serde_json::to_string_pretty(&json_value).unwrap()
    );

    // Extract method
    let method = match json_value.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from("LSP request missing 'method' field"))
                .unwrap());
        }
    };

    let id = json_value.get("id").unwrap_or(&json!(null)).clone();

    // Handle specific LSP methods
    match method {
        "initialize" => {
            // Respond with server capabilities
            let response = LspResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: json!({
                    "capabilities": {
                        "textDocumentSync": {
                            "openClose": true,
                            "change": 1 // Full text sync
                        },
                        "completionProvider": {
                            "triggerCharacters": ["."]
                        },
                        "executeCommandProvider": {
                            "commands": ["copilot.getTools"]
                        },
                        "workspace": {
                            "workspaceFolders": {
                                "supported": true
                            }
                        }
                    },
                    "serverInfo": {
                        "name": "MCP Tools Server",
                        "version": "0.1.0"
                    }
                }),
            };

            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_string(&response).unwrap()))
                .unwrap());
        }
        "initialized" => {
            // No response needed for notification
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::empty())
                .unwrap());
        }
        "shutdown" => {
            // Simple response with null result
            let response = LspResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Value::Null,
            };

            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_string(&response).unwrap()))
                .unwrap());
        }
        "exit" => {
            // No response needed
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .body(Body::empty())
                .unwrap());
        }
        "copilot/getTools" | "workspace/executeCommand" => {
            // Check if this is a getTools command
            let is_tools_command = if method == "workspace/executeCommand" {
                // Check if params has a command field with value "copilot.getTools"
                match json_value
                    .get("params")
                    .and_then(|p| p.get("command"))
                    .and_then(|c| c.as_str())
                {
                    Some("copilot.getTools") => true,
                    _ => false,
                }
            } else {
                true // Direct copilot/getTools call
            };

            if is_tools_command {
                // Return tools in OpenAI format
                let tools = get_tools_as_openai_format(agent_channels).await;

                let response = LspResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: if method == "workspace/executeCommand" {
                        json!({ "tools": tools })
                    } else {
                        json!({ "tools": tools })
                    },
                };

                println!(
                    "Returning tools: {}",
                    serde_json::to_string_pretty(&tools).unwrap()
                );

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&response).unwrap()))
                    .unwrap());
            } else {
                // Handle other commands
                println!("Unknown command in workspace/executeCommand");
                let error_response = LspErrorResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    error: LspError {
                        code: -32601,
                        message: "Command not supported".to_string(),
                    },
                };

                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_string(&error_response).unwrap()))
                    .unwrap());
            }
        }
        "copilot/executeFunction" => {
            // Get function name and params from request
            let params = match json_value.get("params") {
                Some(p) => p,
                None => {
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from("Missing params in executeFunction request"))
                        .unwrap());
                }
            };

            let function_name = match params.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => {
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from(
                            "Missing function name in executeFunction params",
                        ))
                        .unwrap());
                }
            };

            let args = match params.get("arguments") {
                Some(a) => a,
                None => {
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from("Missing arguments in executeFunction params"))
                        .unwrap());
                }
            };

            println!("Executing function: {} with args: {}", function_name, args);

            // Parse arguments (expecting a message parameter)
            let message = match args.get("message").and_then(|m| m.as_str()) {
                Some(m) => m,
                None => {
                    return Ok(Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from(
                            "Missing 'message' parameter in function arguments",
                        ))
                        .unwrap());
                }
            };

            // Create an MCP request
            let (response_tx, response_rx) = std::sync::mpsc::channel::<String>();
            let request_id = format!("req-{}", uuid::Uuid::new_v4());

            // Store the response channel
            {
                let mut response_channels = MCP_RESPONSE_CHANNELS.lock().unwrap();
                response_channels.insert(request_id.clone(), response_tx);
            }

            // Get the agent's channel
            let agent_tx = {
                let channels = agent_channels.lock().unwrap();
                match channels.get(function_name) {
                    Some(tx) => tx.clone(),
                    None => {
                        return Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Body::from(format!("Agent '{}' not found", function_name)))
                            .unwrap());
                    }
                }
            };

            // Send message to the agent
            let function = "Run".to_string(); // Default function name
            let mcp_message = format!("mcp_request:{}:{}", request_id, message);
            if let Err(e) = agent_tx.send((Some(mcp_message), function)) {
                return Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from(format!(
                        "Failed to send message to agent: {}",
                        e
                    )))
                    .unwrap());
            }

            // Wait for response with timeout
            let agent_response = match wait_for_response(response_rx, 120) {
                Some(resp) => resp,
                None => "Timeout waiting for agent response".to_string(),
            };

            // Clean up
            {
                let mut response_channels = MCP_RESPONSE_CHANNELS.lock().unwrap();
                response_channels.remove(&request_id);

                if let Ok(mut request_ids) = MCP_AGENT_REQUEST_IDS.lock() {
                    let agents_to_clear: Vec<String> = request_ids
                        .iter()
                        .filter(|(_, req_id)| req_id == &&request_id)
                        .map(|(agent_id, _)| agent_id.clone())
                        .collect();

                    for agent_id in agents_to_clear {
                        request_ids.remove(&agent_id);
                    }
                }
            }

            // Return the response
            let response = LspResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: json!({
                    "result": agent_response
                }),
            };

            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_string(&response).unwrap()))
                .unwrap());
        }
        _ => {
            // Handle unknown method
            println!("Unknown LSP method: {}", method);
            let error_response = LspErrorResponse {
                jsonrpc: "2.0".to_string(),
                id,
                error: LspError {
                    code: -32601,
                    message: format!("Method not found: {}", method),
                },
            };

            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_string(&error_response).unwrap()))
                .unwrap());
        }
    }
}

// Handler for GET /agents endpoint to list all available agents
async fn handle_list_agents(
    agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
) -> Result<Response<Body>, Infallible> {
    // Get the list of registered agent IDs
    let agents: Vec<AgentInfo> = {
        let channels = agent_channels.lock().unwrap();
        channels
            .keys()
            .map(|id| {
                // Get agent metadata from global state
                if let Ok(metadata) = crate::mcp_server::MCP_AGENT_METADATA.lock() {
                    if let Some((name, description)) = metadata.get(id) {
                        return AgentInfo {
                            id: id.clone(),
                            name: name.clone(),
                            description: description.clone(),
                        };
                    }
                }
                // Fallback if metadata is not available
                AgentInfo {
                    id: id.clone(),
                    name: format!("Agent {}", id),
                    description: "No description available".to_string(),
                }
            })
            .collect()
    };

    // Convert to JSON
    match serde_json::to_string(&agents) {
        Ok(json) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json))
            .unwrap()),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!("Failed to serialize agent list: {}", e)))
            .unwrap()),
    }
}

// OpenAI function format structs for GitHub Copilot
#[derive(Serialize, Debug)]
struct ToolParameter {
    #[serde(rename = "type")]
    param_type: String,
    description: String,
}

#[derive(Serialize, Debug)]
struct ToolParameters {
    #[serde(rename = "type")]
    param_type: String,
    properties: HashMap<String, ToolParameter>,
    required: Vec<String>,
}

#[derive(Serialize, Debug)]
struct ToolDefinition {
    name: String,
    description: String,
    parameters: ToolParameters,
}

// Handler for GET /tools endpoint to list all available agents in OpenAI function format
async fn handle_tools_list(
    agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
) -> Result<Response<Body>, Infallible> {
    let tools = get_tools_as_openai_format(agent_channels).await;

    // Convert to JSON
    match serde_json::to_string(&tools) {
        Ok(json) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json))
            .unwrap()),
        Err(e) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(format!(
                "Failed to serialize tool definitions: {}",
                e
            )))
            .unwrap()),
    }
}

// Helper function to get tools in OpenAI format
async fn get_tools_as_openai_format(
    agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
) -> Vec<ToolDefinition> {
    // Get the list of registered agent IDs
    let channels = agent_channels.lock().unwrap();
    channels
        .keys()
        .filter_map(|id| {
            // Get agent metadata from global state
            if let Ok(metadata) = crate::mcp_server::MCP_AGENT_METADATA.lock() {
                if let Some((name, description)) = metadata.get(id) {
                    // Create simple parameter for the agent's message
                    let mut properties = HashMap::new();
                    properties.insert(
                        "message".to_string(),
                        ToolParameter {
                            param_type: "string".to_string(),
                            description: format!("Message to send to the {} agent", name),
                        },
                    );

                    // Use a more human-friendly function name but preserve the ID for lookup
                    let display_name = name.replace(" ", "_").to_lowercase();

                    return Some(ToolDefinition {
                        name: id.clone(), // Keep using the ID as the function name for consistency
                        description: format!("{} - {}", name, description),
                        parameters: ToolParameters {
                            param_type: "object".to_string(),
                            properties,
                            required: vec!["message".to_string()],
                        },
                    });
                }
            }
            None
        })
        .collect()
}

fn wait_for_response(rx: Receiver<String>, timeout_seconds: u64) -> Option<String> {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_seconds);
    let mut attempts = 0;

    println!(
        "MCP server waiting for response (timeout: {}s)...",
        timeout_seconds
    );

    // Default timeout of 30 seconds may not be enough when waiting for finalresult
    // Actual timeout is determined by the parameter
    while start.elapsed() < timeout {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(response) => {
                println!(
                    "MCP server received response after {:.2}s",
                    start.elapsed().as_secs_f32()
                );
                return Some(response);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                attempts += 1;
                if attempts % 10 == 0 {
                    // Log every ~5 seconds
                    println!(
                        "MCP server still waiting for response ({:.2}s elapsed)...",
                        start.elapsed().as_secs_f32()
                    );
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                println!("MCP server response channel disconnected!");
                return None;
            }
        }
    }

    println!(
        "MCP server timed out after {}s waiting for response",
        timeout_seconds
    );
    None // Timeout
}

// Helper function to send agent responses back to the MCP server
pub fn send_mcp_response(request_id: &str, response: String) -> Result<(), String> {
    let channels = MCP_RESPONSE_CHANNELS
        .lock()
        .map_err(|e| format!("Failed to lock channels: {:?}", e))?;
    if let Some(tx) = channels.get(request_id) {
        tx.send(response)
            .map_err(|e| format!("Failed to send response: {}", e))
    } else {
        Err(format!("Request ID '{}' not found", request_id))
    }
}
