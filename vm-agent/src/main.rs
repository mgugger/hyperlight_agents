use std::process::{Command, Stdio};
use std::io::{Read, Write};
use serde::{Deserialize, Serialize};
use vsock::VsockListener;

// Simple command structure expected by the host
#[derive(Debug, Serialize, Deserialize)]
struct CommandRequest {
    command: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CommandResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn execute_command(command: &str) -> CommandResponse {
    println!("Executing command: {}", command);
    
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    
    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);
            
            println!("Command completed with exit code {}", exit_code);
            
            CommandResponse {
                exit_code,
                stdout,
                stderr,
            }
        }
        Err(e) => {
            eprintln!("Failed to execute command: {}", e);
            CommandResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Failed to execute command: {}", e),
            }
        }
    }
}

fn handle_connection(mut stream: vsock::VsockStream) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== NEW CONNECTION HANDLER STARTED ===");
    
    // Remove the read timeout to handle non-blocking operations manually
    match stream.set_read_timeout(None) {
        Ok(_) => println!("Read timeout disabled successfully"),
        Err(e) => {
            eprintln!("Failed to set read timeout: {}", e);
            return Err(e.into());
        }
    }
    
    let mut buffer = [0; 4096];
    let mut total_message = String::new();
    let read_timeout = std::time::Duration::from_secs(10);
    let start_time = std::time::Instant::now();
    let mut read_attempts = 0;

    println!("Starting read loop...");

    // Loop to handle partial reads and WouldBlock errors
    loop {
        read_attempts += 1;
        println!("Read attempt #{}", read_attempts);
        
        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("Connection closed by client (read returned 0)");
                break;
            }
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buffer[..n]);
                total_message.push_str(&chunk);
                println!("SUCCESS: Received {} bytes, total: {} bytes", n, total_message.len());
                println!("Received chunk: '{}'", chunk);
                println!("Total message so far: '{}'", total_message);
                
                // Try to parse as complete JSON
                println!("Attempting to parse JSON...");
                match serde_json::from_str::<CommandRequest>(&total_message) {
                    Ok(request) => {
                        println!("SUCCESS: JSON parsed successfully");
                        println!("Executing command: '{}'", request.command);
                        let response = execute_command(&request.command);
                        let response_json = serde_json::to_string(&response)?;
                        
                        println!("Sending response: {}", response_json);
                        match stream.write_all(response_json.as_bytes()) {
                            Ok(_) => {
                                println!("Response written to stream");
                                match stream.flush() {
                                    Ok(_) => {
                                        println!("Response flushed successfully");
                                        // Don't wait - let the connection close naturally
                                        // The host will detect the connection closure and parse the complete response
                                        println!("Connection handler will now close");
                                    }
                                    Err(e) => eprintln!("Failed to flush response: {}", e),
                                }
                            }
                            Err(e) => eprintln!("Failed to send response: {}", e),
                        }
                        break;
                    }
                    Err(e) => {
                        println!("JSON parse failed: {} - continuing to read more data", e);
                    }
                }
                
                // Reset buffer for next read
                buffer = [0; 4096];
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                println!("WouldBlock error - no data available yet (elapsed: {:?})", start_time.elapsed());
                if start_time.elapsed() > read_timeout {
                    println!("TIMEOUT: Read timeout reached, sending error response");
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
                println!("Sleeping 50ms before next read attempt...");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                println!("ERROR: Read error - {} (kind: {:?})", e, e.kind());
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

    // If we accumulated data but couldn't parse it as JSON, send error
    if !total_message.is_empty() && !total_message.trim().is_empty() {
        if serde_json::from_str::<CommandRequest>(&total_message).is_err() {
            println!("FINAL ERROR: Failed to parse accumulated JSON: '{}'", total_message);
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
    
    println!("=== CONNECTION HANDLER FINISHED ===");
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== VM AGENT STARTING ===");
    println!("Starting VM Agent listening on VSOCK port 1234");
    
    // Check system information
    println!("Checking system capabilities...");
    if std::path::Path::new("/dev/vsock").exists() {
        println!("✓ VSOCK device found at /dev/vsock");
    } else {
        println!("⚠ WARNING: VSOCK device not found at /dev/vsock");
    }
    
    // Check what VSOCK modules are actually loaded
    println!("Checking loaded VSOCK modules...");
    if let Ok(output) = std::process::Command::new("lsmod").output() {
        let lsmod_output = String::from_utf8_lossy(&output.stdout);
        if lsmod_output.contains("vsock") {
            println!("✓ VSOCK modules are loaded:");
            for line in lsmod_output.lines() {
                if line.contains("vsock") {
                    println!("  {}", line);
                }
            }
        } else {
            println!("⚠ No VSOCK modules found in lsmod output");
        }
    }
    
    // Try to load VSOCK modules
    println!("Attempting to load VSOCK modules...");
    if let Ok(output) = std::process::Command::new("modprobe").arg("vsock").output() {
        println!("modprobe vsock result: {} (stderr: {})", 
                output.status.success(), 
                String::from_utf8_lossy(&output.stderr));
    }
    if let Ok(output) = std::process::Command::new("modprobe").arg("vmw_vsock_virtio_transport").output() {
        println!("modprobe vmw_vsock_virtio_transport result: {} (stderr: {})", 
                output.status.success(),
                String::from_utf8_lossy(&output.stderr));
    }
    
    // Attempt to bind VSOCK listener
    println!("Attempting to bind VSOCK listener...");
    match vsock::VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, 1234) {
        Ok(listener) => {
            println!("✓ VSOCK listener bound successfully on port 1234");
            println!("Entering connection accept loop...");
            
            let mut connection_count = 0;
            for stream in listener.incoming() {
                connection_count += 1;
                println!(">>> INCOMING CONNECTION #{} <<<", connection_count);
                match stream {
                    Ok(stream) => {
                        println!("✓ New VSOCK connection accepted (connection #{})", connection_count);
                        // Handle each connection in current thread for easier debugging
                        if let Err(e) = handle_connection(stream) {
                            eprintln!("✗ Error handling connection #{}: {}", connection_count, e);
                        }
                        println!("Connection #{} handling completed, waiting for next connection...", connection_count);
                    }
                    Err(e) => {
                        eprintln!("✗ Error accepting connection #{}: {}", connection_count, e);
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("✗ FAILED to bind VSOCK listener on port 1234: {}", e);
            eprintln!("VM Agent will exit");
            return Err(e.into());
        }
    }
    
    println!("=== VM AGENT EXITING ===");
    Ok(())
}
