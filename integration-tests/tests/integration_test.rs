use async_trait::async_trait;
use log;
use rust_mcp_sdk::mcp_client::{client_runtime, ClientHandler, ClientRuntime};
use rust_mcp_sdk::schema::{
    CallToolRequestParams, ClientCapabilities, ContentBlock, Implementation,
    InitializeRequestParams, LATEST_PROTOCOL_VERSION,
};
use rust_mcp_sdk::{ClientSseTransport, ClientSseTransportOptions, McpClient};
use serde_json::json;
use std::io::{self};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub struct MyClientHandler;

#[async_trait]
impl ClientHandler for MyClientHandler {}

/// Helper function to build the guest environment
fn build_guest() -> io::Result<()> {
    let root_dir = Path::new("../");
    let status = Command::new("cargo")
        .current_dir(root_dir)
        .args(&["run", "--bin", "xtask", "build-guest"])
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
async fn start_host() -> io::Result<Child> {
    let root_dir = Path::new("../");

    // First, build the host executable to ensure it's up-to-date
    log::info!("Building host executable...");
    let build_status = Command::new("cargo")
        .current_dir(root_dir)
        .args(&["build", "--package", "hyperlight-agents-host"])
        .status()?;

    if !build_status.success() {
        return Err(io::Error::new(io::ErrorKind::Other, "Failed to build host"));
    }
    log::info!("Host executable built successfully.");

    // Then, run the built executable
    let host_executable = Path::new("./target/debug/hyperlight-agents-host");
    let mut command = Command::new(host_executable);
    command.current_dir(root_dir);
    command.env("RUST_LOG", "debug,hyperlight_host=info");

    // Create a new process group for the child process to ensure that signals
    // are correctly propagated to the host and its subprocesses.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()?;
            Ok(())
        });
    }

    log::info!("Starting host executable...");
    let child = command.spawn()?;
    tokio::time::sleep(Duration::from_secs(5)).await; // Allow host to initialize
    Ok(child)
}

/// A guard to ensure the host process is properly stopped
struct HostGuard {
    child: Option<Child>,
}

impl HostGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            log::info!("Stopping host...");
            let _ = stop_host(&mut child); // Call your stop_host function
        }
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        log::info!("HostGuard is being dropped. Attempting to stop the host...");
        self.stop();
    }
}

/// Helper function to stop the host server gracefully
fn stop_host(child: &mut Child) -> io::Result<()> {
    log::info!("Sending SIGINT signal to the host process group...");
    let pgid = nix::unistd::Pid::from_raw(-(child.id() as i32)); // Negative PID targets the process group
    nix::sys::signal::kill(pgid, nix::sys::signal::Signal::SIGINT)?;

    // Wait for the host process to terminate
    log::info!("Waiting for the host process to terminate...");
    match child.wait() {
        Ok(status) => {
            log::info!("Host process terminated with status: {:?}", status);
        }
        Err(e) => {
            log::info!("Failed to wait for host process termination: {:?}", e);
        }
    }

    // Check if the process group is still running
    let output = Command::new("ps").arg("-o").arg("pid,pgid,comm").output()?;
    let ps_output = String::from_utf8_lossy(&output.stdout);
    log::info!("Process group status:\n{}", ps_output);

    if ps_output.contains(&pgid.to_string()) {
        log::info!("Process group is still running. Sending SIGKILL...");
        nix::sys::signal::kill(pgid, nix::sys::signal::Signal::SIGKILL)?;
        log::info!("SIGKILL signal sent to the process group.");
    }

    // Perform emergency cleanup for any orphaned Firecracker processes
    log::info!("Performing emergency cleanup for orphaned Firecracker processes...");
    emergency_cleanup()?;

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

async fn execute_command(client: &Arc<ClientRuntime>, command: &str, action: &str) -> String {
    println!("Sending command: {}", command);
    let params = json!({"action": action, "vm_id": "integration_test_vm", "command": command})
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
            output
        }
        Err(e) => {
            panic!("Failed to call tool: {}", e);
        }
    }
}

/// Integration test for the workspace
#[tokio::test]
async fn integration_test() {
    // Step 0: Build the guest
    build_guest().expect("Failed to build guest");

    // Step 2: Run the host
    let mut host_guard = HostGuard::new(start_host().await.expect("Failed to start host"));

    // Allow the host some time to initialize
    tokio::time::sleep(Duration::from_secs(5)).await;

    let client_details = InitializeRequestParams {
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            title: Some("integration-tests-client".into()),
            name: "integration-tests-client".into(),
            version: "0.1.0".into(),
        },
        protocol_version: LATEST_PROTOCOL_VERSION.to_string(),
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

    // let vm agent startup fully
    tokio::time::sleep(Duration::from_secs(5)).await;

    // execute vm command
    let command = "free -m";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res.contains("buff/cache"),
        "Expected \"buff/cache\" response for command \"{}\", got {:?}",
        command,
        res
    );

    // execute vm command
    let command = "df -h";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res.contains("/dev/root"),
        "Expected \"/dev/root\" response for command \"{}\", got {:?}",
        command,
        res
    );

    // test http call
    let command = "curl http://www.google.com/generate_204";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res == "",
        "Expected empty response for command \"{}\", got {:?}",
        command,
        res
    );
    // test https call
    let command = "curl https://www.google.com/generate_204";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res == "",
        "Expected empty response for command \"{}\", got {:?}",
        command,
        res
    );

    let command = "which caddy";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res.contains("/usr/sbin/caddy"),
        "Expected /usr/sbin/caddy for command \"{}\", got {:?}",
        command,
        res
    );

    // let command = "apk add --no-cache caddy";
    // let res = execute_command(&client, command, "execute_vm_command").await;
    // assert!(
    //     res.contains("OK:"),
    //     "Expected OK response for command \"{}\", got {:?}",
    //     command,
    //     res
    // );

    // // create index.html
    let command = "echo 'Hello from Caddy' > index.html";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res.trim().is_ascii(),
        "Expected empty response for command \"{}\", got {:?}",
        command,
        res
    );

    // // // run caddy
    let command = "caddy file-server --listen :9999 --browse=false";
    let res = execute_command(&client, &command, "spawn_command").await;
    assert!(
        res.contains("cmd_"),
        "Expected cmd_ response for command \"{}\", got {:?}",
        command,
        res
    );

    tokio::time::sleep(Duration::from_secs(1)).await;

    // // // check caddy process is spawned
    let command = "ps aux";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res.trim().contains("caddy"),
        "Expected spawned command {}, got {:?}",
        command,
        res
    );

    // check output
    let command = "curl -s http://localhost:9999/";
    let res = execute_command(&client, command, "execute_vm_command").await;
    assert!(
        res.contains("Hello from Caddy"),
        "Expected cmd_ response for command \"{}\", got {:?}",
        command,
        res
    );

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
    host_guard.stop();
}
