use std::collections::HashMap;
use std::io::Read;
use std::io::Write;
mod logger;
mod command_execution;
use command_execution::{execute_command, CommandResponse};
mod http_proxy;
use http_proxy::HttpProxyResponse;
use http_proxy::start_http_proxy_server;
use serde::{Serialize, Deserialize};
use hyperlight_agents_common::VmCommandMode;
use hyperlight_agents_common::VmCommand;

/// VsockRequest enum for proxy requests
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VsockRequest {
    Command(VmCommand),
    HttpProxy(http_proxy::HttpProxyRequest),
}

/// VsockResponse enum for proxy responses
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VsockResponse {
    Command(command_execution::CommandResponse),
    HttpProxy(HttpProxyResponse),
    SpawnedProcess(command_execution::SpawnedProcessInfo),
    SpawnedProcessList(Vec<command_execution::SpawnedProcessInfo>),
    StoppedProcess(command_execution::StopProcessResponse),
}

fn handle_connection(mut stream: vsock::VsockStream) -> Result<(), Box<dyn std::error::Error>> {
    log::debug!("=== NEW CONNECTION HANDLER STARTED ===");

    // Remove the read timeout to handle non-blocking operations manually
    match stream.set_read_timeout(None) {
        Ok(_) => log::debug!("Read timeout disabled successfully"),
        Err(e) => {
            log::error!("Failed to set read timeout: {}", e);
            return Err(e.into());
        }
    }

    let mut buffer = [0; 4096];
    let mut total_message = String::new();
    let read_timeout = std::time::Duration::from_secs(10);
    let start_time = std::time::Instant::now();
    let mut read_attempts = 0;
    let mut response_sent = false;

    log::debug!("Starting read loop...");

    // Loop to handle partial reads and WouldBlock errors
    loop {
        read_attempts += 1;
        log::debug!("Read attempt #{}", read_attempts);

        match stream.read(&mut buffer) {
            Ok(0) => {
                log::debug!("Connection closed by client (read returned 0)");
                break;
            }
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buffer[..n]);
                total_message.push_str(&chunk);
                log::debug!(
                    "SUCCESS: Received {} bytes, total: {} bytes",
                    n,
                    total_message.len()
                );
                log::debug!("Received chunk: '{}'", chunk);
                log::debug!("Total message so far: '{}'", total_message);

                // Try to parse as complete JSON
                log::debug!("Attempting to parse JSON...");

                // First try to parse as new VsockRequest format
                if let Ok(request) = serde_json::from_str::<VsockRequest>(&total_message) {
                    log::debug!("SUCCESS: JSON parsed as VsockRequest");
                    let response = match request {
                        VsockRequest::Command(vm_cmd) => {
                            log::debug!("Received Command: '{:?}'", vm_cmd);
                            match vm_cmd.mode {
                                VmCommandMode::Foreground => {
                                    // Foreground: run and wait for result
                                    let cmd_response = command_execution::execute_command(&vm_cmd.command, 15);
                                    VsockResponse::Command(cmd_response)
                                }
                                VmCommandMode::Spawn => {
                                    // Background: spawn and return process info
                                    let result = command_execution::spawn_command_struct(&vm_cmd);
                                    match result {
                                        Some(info) => VsockResponse::SpawnedProcess(info),
                                        None => VsockResponse::StoppedProcess(command_execution::StopProcessResponse {
                                            id: 0,
                                            exit_code: -1,
                                            stdout: String::new(),
                                            stderr: "Failed to spawn process".to_string(),
                                        }),
                                    }
                                }
                            }
                        }
                        VsockRequest::HttpProxy(proxy_req) => {
                            log::debug!(
                                "Processing HTTP proxy request: {} {}",
                                proxy_req.method, proxy_req.url
                            );
                            // For now, return an error since we need the host to handle this
                            let error_response = HttpProxyResponse {
                                status_code: 500,
                                headers: HashMap::new(),
                                body: b"HTTP proxy not yet implemented in VM agent".to_vec(),
                                error: Some("HTTP proxy functionality requires host-side implementation".to_string()),
                            };
                            VsockResponse::HttpProxy(error_response)
                        }

                    };
                    let response_json = serde_json::to_string(&response)?;

                        log::debug!("Sending response: {}", response_json);
                        match stream.write_all(response_json.as_bytes()) {
                            Ok(_) => {
                                log::debug!("Response written to stream");
                                match stream.flush() {
                                    Ok(_) => {
                                        log::debug!("Response flushed successfully");
                                        // Don't wait - let the connection close naturally
                                        // The host will detect the connection closure and parse the complete response
                                        log::debug!("Connection handler will now close");
                                    }
                                    Err(e) => log::error!("Failed to flush response: {}", e),
                                }
                            }
                            Err(e) => log::error!("Failed to send response: {}", e),
                        }
                        response_sent = true;
                        break;
                } else {
                    log::debug!("JSON parse failed");
                }

                // Reset buffer for next read
                buffer = [0; 4096];
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                log::debug!(
                    "WouldBlock error - no data available yet (elapsed: {:?})",
                    start_time.elapsed()
                );
                if start_time.elapsed() > read_timeout {
                    log::error!("TIMEOUT: Read timeout reached, sending error response");
                    let error_response = CommandResponse {
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: "Read timeout waiting for complete command".to_string(),
                    };
                    let response_json = serde_json::to_string(&error_response)?;
                    let _ = stream.write_all(response_json.as_bytes());
                    let _ = stream.flush();
                    break;
                }
                // Wait a bit before trying again
                log::debug!("Sleeping 50ms before next read attempt...");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                log::debug!("ERROR: Read error - {} (kind: {:?})", e, e.kind());
                // Send an error response if possible
                let error_response = CommandResponse {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("Read error: {}", e),
                };
                if let Ok(response_json) = serde_json::to_string(&error_response) {
                    let _ = stream.write_all(response_json.as_bytes());
                    let _ = stream.flush();
                }
                break;
            }
        }
    }

    // If we accumulated data but couldn't parse it as JSON, send error (only if no response was sent)
    if !response_sent && !total_message.is_empty() && !total_message.trim().is_empty() {
        if serde_json::from_str::<VsockRequest>(&total_message).is_err() {
            log::error!(
                "FINAL ERROR: Failed to parse accumulated JSON: '{}'",
                total_message
            );
            // Try to send error as legacy format first (more likely to work)
            let error_response = CommandResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Invalid JSON request: {}", total_message),
            };
            if let Ok(response_json) = serde_json::to_string(&error_response) {
                let _ = stream.write_all(response_json.as_bytes());
                let _ = stream.flush();
            }
        }
    }

    log::debug!("=== CONNECTION HANDLER FINISHED ===");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_level = std::env::var("RUST_LOG")
        .ok()
        .and_then(|lvl| lvl.parse::<log::LevelFilter>().ok())
        .unwrap_or(log::LevelFilter::Info);
    let vsock_logger = logger::bounded_logger::BoundedVsockLogger::init(1236).await;
    logger::bounded_logger::init_combined_logger(vsock_logger.clone(), log_level).expect("Failed to initialize logger");

    log::info!("=== VM AGENT STARTING ===");
    log::debug!("Starting VM Agent with VSOCK server on port 1234 and HTTP proxy on port 8080");

    // Start HTTP proxy server in background
    let proxy_handle = tokio::spawn(async {
        if let Err(e) = start_http_proxy_server().await {
            log::error!("HTTP proxy server error: {}", e);
        }
    });

    // Give the HTTP proxy server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Start VSOCK server in a separate task
    let vsock_handle = tokio::task::spawn_blocking(|| {
        // Check system information
        log::debug!("Checking system capabilities...");
        if std::path::Path::new("/dev/vsock").exists() {
            log::debug!("✓ VSOCK device found at /dev/vsock");
        } else {
            log::debug!("⚠ WARNING: VSOCK device not found at /dev/vsock");
        }

        // Check what VSOCK modules are actually loaded
        log::debug!("Checking loaded VSOCK modules...");
        if let Ok(output) = std::process::Command::new("lsmod").output() {
            let lsmod_output = String::from_utf8_lossy(&output.stdout);
            if lsmod_output.contains("vsock") {
                log::debug!("✓ VSOCK modules are loaded:");
                for line in lsmod_output.lines() {
                    if line.contains("vsock") {
                        log::debug!("  {}", line);
                    }
                }
            } else {
                log::debug!("⚠ No VSOCK modules found in lsmod output");
            }
        }

        // Try to load VSOCK modules
        log::debug!("Attempting to load VSOCK modules...");
        if let Ok(output) = std::process::Command::new("modprobe").arg("vsock").output() {
            log::debug!(
                "modprobe vsock result: {} (stderr: {})",
                output.status.success(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if let Ok(output) = std::process::Command::new("modprobe")
            .arg("vmw_vsock_virtio_transport")
            .output()
        {
            log::debug!(
                "modprobe vmw_vsock_virtio_transport result: {} (stderr: {})",
                output.status.success(),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Attempt to bind VSOCK listener
        log::debug!("Attempting to bind VSOCK listener...");
        match vsock::VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, 1234) {
            Ok(listener) => {
                log::debug!("✓ VSOCK listener bound successfully on port 1234");
                log::debug!("Entering connection accept loop...");

                let mut connection_count = 0;
                for stream in listener.incoming() {
                    connection_count += 1;
                    log::debug!(">>> INCOMING CONNECTION #{} <<<", connection_count);
                    match stream {
                        Ok(stream) => {
                            log::debug!(
                                "✓ New VSOCK connection accepted (connection #{})",
                                connection_count
                            );
                            // Handle each connection in current thread for easier debugging
                            if let Err(e) = handle_connection(stream) {
                                log::error!("✗ Error handling connection #{}: {}", connection_count, e);
                            }
                            log::debug!(
                                "Connection #{} handling completed, waiting for next connection...",
                                connection_count
                            );
                        }
                        Err(e) => {
                            log::error!("✗ Error accepting connection #{}: {}", connection_count, e);
                        }
                    }
                }
                Ok(())
            }
            Err(e) => {
                log::error!("✗ FAILED to bind VSOCK listener on port 1234: {}", e);
                Err(e)
            }
        }
    });

    // Wait for either task to complete
    log::debug!("Waiting for proxy and vsock tasks to complete...");
    tokio::select! {
        proxy_result = proxy_handle => {
            match proxy_result {
                Ok(_) => log::debug!("HTTP proxy server completed"),
                Err(e) => log::error!("HTTP proxy server task error: {}", e),
            }
        }
        vsock_result = vsock_handle => {
            match vsock_result {
                Ok(Ok(_)) => log::debug!("VSOCK server completed"),
                Ok(Err(e)) => {
                    log::error!("VSOCK server error: {}", e);
                    return Err(e.into());
                }
                Err(e) => {
                    log::error!("VSOCK server task error: {}", e);
                    return Err(e.into());
                }
            }
        }
    }
    log::debug!("Proxy and vsock tasks completed");
    Ok(())
}
