use async_trait::async_trait;
use rust_mcp_sdk::error::SdkResult;
use rust_mcp_sdk::mcp_client::{client_runtime, ClientHandler};
use rust_mcp_sdk::schema::{
    CallToolRequestParams, ClientCapabilities, ContentBlock, Implementation,
    InitializeRequestParams, LATEST_PROTOCOL_VERSION,
};
use rust_mcp_sdk::{ClientSseTransport, ClientSseTransportOptions, McpClient};
use serde_json::json;
use std::io::{self};
use std::path::Path;
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

pub struct MyClientHandler;

#[async_trait]
impl ClientHandler for MyClientHandler {
    // Implement required methods here if needed
}

/// Helper function to build the guest environment
fn build_guest() -> io::Result<()> {
    let root_dir = Path::new("../guest");
    let status = Command::new("cargo")
        .current_dir(root_dir)
        .args(&["build", "--release"])
        .status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Failed to build guest",
        ));
    }
    Ok(())
}

/// Helper function to start the host server
fn start_host() -> io::Result<Child> {
    let root_dir = Path::new("../");
    let child = Command::new("cargo")
        .current_dir(root_dir)
        .args(&["run", "-p", "xtask", "--", "run-host"])
        .spawn()?;
    thread::sleep(Duration::from_secs(5)); // Allow host to initialize
    Ok(child)
}

/// Helper function to stop the host server gracefully
fn stop_host(child: &mut Child) -> io::Result<()> {
    child.kill()?;
    Ok(())
}

/// Helper function to perform emergency cleanup
fn emergency_cleanup() -> io::Result<()> {
    let output = Command::new("pgrep")
        .args(&["-f", "firecracker"])
        .output()?;
    if output.status.success() {
        let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect();
        for pid in pids {
            Command::new("kill")
                .args(&["-9", &pid.to_string()])
                .status()?;
        }
    }
    Ok(())
}

/// Integration test for the workspace
#[tokio::test]
async fn integration_test() {
    // Step 0: Build the guest
    build_guest().expect("Failed to build guest");

    // Step 2: Run the host
    let mut host_child = start_host().expect("Failed to start host");

    // Allow the host some time to initialize
    thread::sleep(Duration::from_secs(5));

    // Step 3: Run MCP client and connect to the host MCP server
    // Step 3.1: List tools on the MCP server
    // async fn interact_with_mcp_server() -> SdkResult<()> {

    //     println!("Available tools:");
    //     for (index, tool) in tools.iter().enumerate() {
    //         println!(
    //             "{}. {}: {}",
    //             index + 1,
    //             tool.name,
    //             tool.description.clone().unwrap_or_default()
    //         );
    //     }

    //     let params = json!({"a": 100, "b": 28}).as_object().unwrap().clone();
    //     let request = CallToolRequestParams {
    //         name: "add".to_string(),
    //         arguments: Some(params),
    //     };
    //     let result = client.call_tool(request).await?;
    //     println!(
    //         "Tool result: {}",
    //         result.content.first().unwrap().as_text_content()?.text
    //     );

    //     Ok(())
    // }
    //

    let client_details = InitializeRequestParams {
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            title: None,
            name: "integration-tests-client".into(),
            version: "0.1.0".into(),
        },
        protocol_version: "2024-11-05".into(),
    };

    let transport = ClientSseTransport::new(
        "http://127.0.0.1:3000/sse",
        ClientSseTransportOptions::default(),
    )
    .unwrap();
    let handler = MyClientHandler {};
    let client = client_runtime::create_client(client_details, transport, handler);

    let _res = client.clone().start().await;

    let tools = client.list_tools(None).await;
    match tools {
        Ok(_) => {
            // Process the tools
        }
        Err(e) => {
            panic!("Failed to list tools: {}", e);
        }
    }

    // create vm
    let params = json!({"action": "create_vm", "vm_id": "integration_test_vm"})
        .as_object()
        .unwrap()
        .clone();
    let request = CallToolRequestParams {
        name: "vm_builder".to_string(),
        arguments: Some(params),
    };
    let result = client.call_tool(request).await;
    match result {
        Ok(_) => {
            // Process the result
        }
        Err(e) => {
            panic!("Failed to call tool: {}", e);
        }
    }

    // execute vm command
    let params = json!({"action": "execute_vm_command", "vm_id": "integration_test_vm", "command": "curl www.google.ch"})
        .as_object()
        .unwrap()
        .clone();
    let request = CallToolRequestParams {
        name: "vm_builder".to_string(),
        arguments: Some(params),
    };
    let result = client.call_tool(request).await;
    match result {
        Ok(res) => {
            let output = match res.content.first() {
                Some(ContentBlock::TextContent(content)) => content.text.clone(),
                Some(_) => panic!("No content found"),
                None => panic!("No content found"),
            };
            assert!(
                output.contains("content=\"text/html"),
                "Expected HTML content, got {}",
                output
            )
        }
        Err(e) => {
            panic!("Failed to call tool: {}", e);
        }
    }

    // destroy vm
    let params = json!({"action": "destroy_vm", "vm_id": "integration_test_vm"})
        .as_object()
        .unwrap()
        .clone();
    let request = CallToolRequestParams {
        name: "vm_builder".to_string(),
        arguments: Some(params),
    };
    let result = client.call_tool(request).await;
    match result {
        Ok(_) => {
            // Process the result
        }
        Err(e) => {
            panic!("Failed to call tool: {}", e);
        }
    }

    // Step 4: Tear down the host
    stop_host(&mut host_child).expect("Failed to stop host");

    // Perform emergency cleanup if necessary
    emergency_cleanup().expect("Failed to perform emergency cleanup");
}
