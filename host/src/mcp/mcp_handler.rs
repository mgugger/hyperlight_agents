use async_trait::async_trait;
use hyperlight_agents_common::{
    constants,
    traits::agent::{Param, ParamType},
};
use rust_mcp_schema::{
    schema_utils::CallToolError, CallToolRequest, CallToolResult, ListToolsRequest,
    ListToolsResult, RpcError, Tool, ToolInputSchema,
};
use rust_mcp_sdk::{mcp_server::ServerHandler, McpServer};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

use crate::mcp::mcp_server::{MCP_AGENT_REQUEST_IDS, MCP_RESPONSE_CHANNELS};

use super::mcp_server::MCP_AGENT_METADATA;

// Custom server handler for MCP
pub struct HyperlightAgentHandler {
    pub agent_channels: Arc<Mutex<HashMap<String, Sender<(Option<String>, String)>>>>,
}

#[async_trait]
impl ServerHandler for HyperlightAgentHandler {
    // Handle ListToolsRequest, return list of available tools

    async fn handle_list_tools_request(
        &self,
        _request: ListToolsRequest,
        _runtime: &dyn McpServer,
    ) -> Result<ListToolsResult, RpcError> {
        let mut tools = Vec::new();

        if let Ok(metadata) = MCP_AGENT_METADATA.lock() {
            for (agent_id, (name, description, params)) in metadata.iter() {
                let parameters = params_to_tool_input_schema(params.clone());

                tools.push(Tool {
                    title: Some(agent_id.clone()),
                    name: agent_id.clone(),
                    description: Some(format!("{} - {}", name, description)),
                    input_schema: parameters,
                    output_schema: None,
                    annotations: None,
                    meta: None,
                });
            }
        }

        Ok(ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }

    // Handle CallToolRequest, communicate with the agent and return the result
    async fn handle_call_tool_request(
        &self,
        request: CallToolRequest,
        _runtime: &dyn McpServer,
    ) -> Result<CallToolResult, CallToolError> {
        // Extract the tool name from the request
        let tool_name = request.tool_name();

        let request_id = format!("req-{}", uuid::Uuid::new_v4());

        log::debug!("{:?}", request);

        // Log the incoming request
        //log_mcp_request(&tool_name, "message", &request_id);

        // Get the agent's channel
        let agent_tx = {
            let channels = self.agent_channels.lock().unwrap();
            match channels.get(tool_name) {
                Some(tx) => tx.clone(),
                None => {
                    return Err(CallToolError::new(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("Agent '{}' not found", tool_name),
                    )));
                }
            }
        };

        // Create a channel for the response
        let (resp_tx, resp_rx) = oneshot::channel::<String>();
        {
            let mut response_channels = MCP_RESPONSE_CHANNELS.lock().unwrap();
            response_channels.insert(request_id.clone(), resp_tx);
        }

        let parameters = request.params.clone().arguments.unwrap_or_default();

        // Convert parameters to a JSON string to pass to the agent
        let params_json = serde_json::to_string(&parameters).unwrap_or_else(|_| "{}".to_string());

        // Send message to the agent
        let function_name = constants::GuestMethod::Run.as_ref().to_string();
        // Wrap the message with MCP protocol info
        let mcp_message = format!("mcp_request:{}:{}", request_id, params_json);

        // Use .await to fix the Send future error
        if let Err(e) = agent_tx.clone().send((Some(mcp_message), function_name)) {
            return Err(CallToolError::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to send message to agent: {}", e),
            )));
        }

        log::debug!(
            "[REQUEST ID: {}] Processing request for agent '{}'",
            request_id,
            tool_name
        );

        // Wait for response with timeout
        let response = match wait_for_response(resp_rx, 120).await {
            Some(resp) => resp,
            None => {
                return Err(CallToolError::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Timeout waiting for agent response",
                )));
            }
        };

        // Clean up resources
        {
            log::debug!(
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
                    log::debug!("Cleaned up request ID mapping for agent: {}", agent_id);
                }
            }
        }

        // Return the agent's response as text content
        Ok(CallToolResult::text_content(vec![
            rust_mcp_schema::TextContent::new(response, None, None),
        ]))
    }
}

pub fn params_to_tool_input_schema(params: Vec<Param>) -> ToolInputSchema {
    let mut required = Vec::new();
    let mut properties: HashMap<String, Map<String, Value>> = HashMap::new();

    for param in params {
        let name_str = param.name.to_string();
        if param.required {
            required.push(name_str.clone());
        }

        let mut schema_entry = Map::new();

        // Set the "type" field based on ParamType
        schema_entry.insert(
            "type".to_string(),
            Value::String(
                match param.param_type {
                    ParamType::String => "string",
                    ParamType::Integer => "integer",
                    ParamType::Boolean => "boolean",
                    ParamType::Float => "number",
                }
                .to_string(),
            ),
        );

        // Add description if provided
        if let Some(desc) = param.description {
            schema_entry.insert("description".to_string(), Value::String(desc));
        }

        properties.insert(name_str, schema_entry);
    }

    ToolInputSchema::new(required, Some(properties))
}

async fn wait_for_response(rx: oneshot::Receiver<String>, timeout_seconds: u64) -> Option<String> {
    let timeout = Duration::from_secs(timeout_seconds);

    log::debug!(
        "MCP server waiting for response (timeout: {}s)...",
        timeout_seconds
    );

    // Use tokio timeout for the oneshot receiver
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(response)) => {
            log::debug!("MCP server received response");
            Some(response)
        }
        Ok(Err(_)) => {
            log::error!("MCP server response channel was dropped");
            None
        }
        Err(_) => {
            log::error!(
                "MCP server timed out after {}s waiting for response",
                timeout_seconds
            );
            None
        }
    }
}
