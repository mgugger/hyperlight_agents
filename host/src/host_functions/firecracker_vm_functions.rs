use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use vsock::{VsockListener, VsockStream};
// Add these imports for process management and signal handling
use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct VmInstance {
    pub vm_id: String,
    pub cid: u32, // VSOCK Context ID
    pub pid: Option<u32>,
    pub temp_dir: TempDir,
    pub command_sender: mpsc::Sender<VmCommand>,
    pub result_receiver: Arc<Mutex<HashMap<String, mpsc::Sender<VmCommandResult>>>>,
}

#[derive(Debug, Clone)]
pub struct VmCommand {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct VmCommandResult {
    pub id: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

pub struct VmManager {
    instances: Arc<Mutex<HashMap<String, VmInstance>>>,
    next_cid: Arc<Mutex<u32>>,
    vsock_listener: Arc<Mutex<Option<VsockListener>>>,
    shutting_down: Arc<AtomicBool>,
}

impl VmManager {
    pub fn new() -> Self {
        let firecracker_available = Command::new(
            "/home/manuel/firecracker/release-v1.12.1-x86_64/firecracker-v1.12.1-x86_64",
        )
        .arg("--version")
        .output()
        .is_ok();
        if !firecracker_available {
            panic!("Firecracker not detected");
        } else {
            Self {
                instances: Arc::new(Mutex::new(HashMap::new())),
                next_cid: Arc::new(Mutex::new(100)), // Start CIDs from 100
                vsock_listener: Arc::new(Mutex::new(None)),
                shutting_down: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    pub fn start_vsock_server(
        &self,
        port: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, port)?;
        println!("VSOCK server listening on port {}", port);

        *self.vsock_listener.lock().unwrap() = Some(listener);

        let instances = self.instances.clone();
        let listener_clone = VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, port)?;

        thread::spawn(move || {
            for stream in listener_clone.incoming() {
                match stream {
                    Ok(mut stream) => {
                        let instances_clone = instances.clone();
                        thread::spawn(move || {
                            if let Err(e) = Self::handle_vm_connection(&mut stream, instances_clone)
                            {
                                eprintln!("Error handling VM connection: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Error accepting VSOCK connection: {}", e);
                    }
                }
            }
        });

        Ok(())
    }

    fn handle_vm_connection(
        stream: &mut VsockStream,
        instances: Arc<Mutex<HashMap<String, VmInstance>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut buffer = [0; 4096];

        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break, // Connection closed
                Ok(n) => {
                    let message = String::from_utf8_lossy(&buffer[..n]);

                    // Parse incoming message as JSON
                    if let Ok(json_msg) = serde_json::from_str::<Value>(&message) {
                        match json_msg["type"].as_str() {
                            Some("register") => {
                                // VM registering itself
                                let vm_id = json_msg["vm_id"].as_str().unwrap_or("unknown");
                                let cid = json_msg["cid"].as_u64().unwrap_or(0) as u32;

                                println!("VM {} registered with CID {}", vm_id, cid);

                                let response = serde_json::json!({
                                    "type": "register_ack",
                                    "vm_id": vm_id,
                                    "status": "success"
                                });

                                stream.write_all(response.to_string().as_bytes())?;
                            }
                            Some("command_result") => {
                                // VM sending command execution result
                                println!("Received command result: {}", message);

                                // Parse the command result
                                let cmd_result = VmCommandResult {
                                    id: json_msg["id"].as_str().unwrap_or("").to_string(),
                                    exit_code: json_msg["exit_code"].as_i64().unwrap_or(-1) as i32,
                                    stdout: json_msg["stdout"].as_str().unwrap_or("").to_string(),
                                    stderr: json_msg["stderr"].as_str().unwrap_or("").to_string(),
                                    error: json_msg["error"].as_str().map(|s| s.to_string()),
                                };

                                // Find the VM and send result to waiting caller
                                let vm_id = json_msg["vm_id"].as_str().unwrap_or("");
                                let instances = instances.lock().unwrap();
                                if let Some(vm_instance) = instances.get(vm_id) {
                                    let result_receivers =
                                        vm_instance.result_receiver.lock().unwrap();
                                    if let Some(sender) = result_receivers.get(&cmd_result.id) {
                                        let _ = sender.send(cmd_result);
                                    }
                                }
                            }
                            _ => {
                                println!("Unknown message type: {}", message);
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Error reading from VSOCK stream: {}", e);
                    break;
                }
            }
        }

        Ok(())
    }

    pub async fn create_vm(
        &self,
        vm_id: String,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let cid = {
            let mut next_cid = self.next_cid.lock().unwrap();
            let current_cid = *next_cid;
            *next_cid += 1;
            current_cid
        };

        // Create temporary directory for VM-specific files (like sockets)
        let temp_dir = TempDir::new()?;

        // Create command channel for this VM
        let (command_sender, command_receiver) = mpsc::channel::<VmCommand>();

        // Start the Firecracker VM using real images from vm-images directory
        let vm_process = self.start_firecracker_vm(&temp_dir.path(), cid)?;

        let vm_instance = VmInstance {
            vm_id: vm_id.clone(),
            cid,
            pid: vm_process,
            temp_dir,
            command_sender,
            result_receiver: Arc::new(Mutex::new(HashMap::new())),
        };

        // Store the VM instance
        {
            let mut instances = self.instances.lock().unwrap();
            instances.insert(vm_id.clone(), vm_instance);
        }

        // Start command processor for this VM
        self.start_command_processor(vm_id.clone(), command_receiver, cid);

        Ok(format!("VM {} created with CID {}", vm_id, cid))
    }

    fn start_firecracker_vm(
        &self,
        vm_dir: &Path,
        cid: u32,
    ) -> Result<Option<u32>, Box<dyn std::error::Error + Send + Sync>> {
        // Always use the real images from vm-images directory
        let vm_images_dir = Path::new("/home/manuel/git/hyperlight_agents/vm-images");
        let kernel_path = vm_images_dir.join("vmlinux");
        let rootfs_path = vm_images_dir.join("rootfs.ext4");
        let config_path = vm_dir.join("firecracker-config.json");

        // Verify that the real images exist
        if !kernel_path.exists() {
            return Err(
                format!("Real kernel image not found at: {}", kernel_path.display()).into(),
            );
        }
        if !rootfs_path.exists() {
            return Err(
                format!("Real rootfs image not found at: {}", rootfs_path.display()).into(),
            );
        }

        // Create Firecracker configuration using real images
        let config = serde_json::json!({
            "boot-source": {
                "kernel_image_path": kernel_path.to_str().unwrap(),
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off"
            },
            "drives": [{
                "drive_id": "rootfs",
                "path_on_host": rootfs_path.to_str().unwrap(),
                "is_root_device": true,
                "is_read_only": false
            }],
            "machine-config": {
                "vcpu_count": 1,
                "mem_size_mib": 128,
                "smt": false
            },
            "vsock": {
                "guest_cid": cid,
                "uds_path": format!("{}/vsock.sock", vm_dir.display())
            }
        });

        std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;

        // Log what we're using
        println!("Starting Firecracker VM with real images:");
        println!(
            "  Kernel: {} ({} bytes)",
            kernel_path.display(),
            std::fs::metadata(&kernel_path)?.len()
        );
        println!(
            "  Rootfs: {} ({} bytes)",
            rootfs_path.display(),
            std::fs::metadata(&rootfs_path)?.len()
        );
        println!("  Config: {}", config_path.display());
        println!("  Guest CID: {}", cid);

        // Simulate starting Firecracker (for testing without actual firecracker binary)
        match std::env::var("SKIP_FIRECRACKER") {
            Ok(_) => {
                println!("Simulating Firecracker VM start for testing");
                println!("  VM would be started with CID: {}", cid);
                println!("  VSOCK socket would be: {}/vsock.sock", vm_dir.display());
                Ok(Some(12345)) // Fake PID
            }
            Err(_) => {
                // Try to start real Firecracker

                let devnull = File::create("/dev/null").unwrap();
                let mut cmd = Command::new(
                    "/home/manuel/firecracker/release-v1.12.1-x86_64/firecracker-v1.12.1-x86_64",
                );
                cmd.arg("--api-sock")
                    .arg(format!("{}/firecracker.sock", vm_dir.display()))
                    .arg("--config-file")
                    .arg(&config_path)
                    .stdout(devnull.try_clone().unwrap())
                    .stderr(devnull);

                println!(
                    "Starting Firecracker with config: {}",
                    config_path.display()
                );
                match cmd.spawn() {
                    Ok(child) => {
                        println!("Started Firecracker VM with PID: {}", child.id());
                        println!("  VSOCK socket: {}/vsock.sock", vm_dir.display());
                        println!("  Guest CID: {}", cid);

                        // Give VM a moment to start
                        std::thread::sleep(std::time::Duration::from_secs(2));

                        // Check if VSOCK socket was created
                        let vsock_path = format!("{}/vsock.sock", vm_dir.display());
                        println!(
                            "  VSOCK socket created: {}",
                            std::path::Path::new(&vsock_path).exists()
                        );

                        Ok(Some(child.id()))
                    }
                    Err(e) => {
                        eprintln!("Failed to start Firecracker VM: {}", e);
                        println!("To skip Firecracker, set SKIP_FIRECRACKER=1");
                        Ok(None)
                    }
                }
            }
        }
    }

    fn start_command_processor(
        &self,
        vm_id: String,
        receiver: mpsc::Receiver<VmCommand>,
        _cid: u32,
    ) {
        let instances = self.instances.clone();
        let shutting_down = self.shutting_down.clone();

        thread::spawn(move || {
            for command in receiver {
                // Check if we're shutting down
                if shutting_down.load(Ordering::SeqCst) {
                    println!(
                        "Command processor for VM {} shutting down, ignoring command {}",
                        vm_id, command.id
                    );
                    break;
                }

                println!("Processing command {} for VM {}", command.command, vm_id);

                // Get the VSOCK socket path and result sender for this VM and command
                let (vsock_socket_path, result_sender) = {
                    let instances = instances.lock().unwrap();
                    if let Some(vm_instance) = instances.get(&vm_id) {
                        let socket_path =
                            format!("{}/vsock.sock", vm_instance.temp_dir.path().display());

                        // Get the result sender for this specific command (but don't remove it yet)
                        // The sender will be cleaned up by execute_command_in_vm after receiving the result
                        let result_sender = {
                            let result_receivers = vm_instance.result_receiver.lock().unwrap();
                            println!(
                                "DEBUG: Looking for result sender for command ID: {}",
                                command.id
                            );
                            println!(
                                "DEBUG: Available result receiver IDs: {:?}",
                                result_receivers.keys().collect::<Vec<_>>()
                            );
                            let sender = result_receivers.get(&command.id).cloned();
                            if sender.is_some() {
                                println!("DEBUG: Found result sender for command {}", command.id);
                            } else {
                                println!(
                                    "DEBUG: No result sender found for command {}",
                                    command.id
                                );
                            }
                            sender
                        };

                        (socket_path, result_sender)
                    } else {
                        eprintln!("VM {} not found in instances", vm_id);
                        continue;
                    }
                };

                println!(
                    "DEBUG: Processing command {} with result_sender: {}",
                    command.id,
                    result_sender.is_some()
                );
                println!(
                    "Attempting to connect to VSOCK socket: {}",
                    vsock_socket_path
                );

                // Check shutdown again before processing
                if shutting_down.load(Ordering::SeqCst) {
                    println!("Shutdown detected during command processing, stopping");
                    break;
                }

                // Create default error result
                let mut vm_result = VmCommandResult {
                    id: command.id.clone(),
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: None,
                };

                // Check if socket exists
                if !std::path::Path::new(&vsock_socket_path).exists() {
                    eprintln!("VSOCK socket does not exist: {}", vsock_socket_path);
                    vm_result.error =
                        Some(format!("VSOCK socket not found: {}", vsock_socket_path));

                    // Send error result back
                    if let Some(sender) = result_sender {
                        let _ = sender.send(vm_result);
                    }
                    continue;
                }

                // Connect via Unix Domain Socket (Firecracker's VSOCK bridge)
                match UnixStream::connect(&vsock_socket_path) {
                    Ok(mut stream) => {
                        let mut success = false;

                        // Set socket to blocking mode immediately after connecting
                        println!("DEBUG: Setting socket to blocking mode immediately after connection...");
                        stream.set_nonblocking(false).ok();

                        // Send Firecracker VSOCK handshake first
                        let handshake = "CONNECT 1234\n";
                        if let Err(e) = stream.write_all(handshake.as_bytes()) {
                            eprintln!("Failed to send handshake to VM {}: {}", vm_id, e);
                            vm_result.error = Some(format!("Handshake send failed: {}", e));
                        } else {
                            println!("DEBUG: Handshake sent successfully");
                            // Read handshake response
                            let mut handshake_buffer = [0; 256];
                            match stream.read(&mut handshake_buffer) {
                                Ok(n) => {
                                    let handshake_response =
                                        String::from_utf8_lossy(&handshake_buffer[..n]);
                                    println!(
                                        "VSOCK handshake response: {}",
                                        handshake_response.trim()
                                    );
                                    println!("DEBUG: Handshake response received, checking...");

                                    // Check if handshake was successful
                                    if !handshake_response.starts_with("OK") {
                                        eprintln!(
                                            "VSOCK handshake failed for VM {}: {}",
                                            vm_id,
                                            handshake_response.trim()
                                        );
                                        vm_result.error = Some(format!(
                                            "Handshake failed: {}",
                                            handshake_response.trim()
                                        ));
                                    } else {
                                        println!("DEBUG: Handshake successful, proceeding to send command");
                                        success = true;
                                    }
                                }
                                Err(e) => {
                                    eprintln!(
                                        "Failed to read handshake response from VM {}: {}",
                                        vm_id, e
                                    );
                                    vm_result.error = Some(format!("Handshake read failed: {}", e));
                                }
                            }
                        }

                        if success {
                            println!("DEBUG: Entering success block to send command");
                            // Send simple command format expected by vm-agent
                            let command_json = serde_json::json!({
                                "command": format!("{} {}", command.command, command.args.join(" "))
                            });

                            let command_str = command_json.to_string();
                            println!("DEBUG: Command JSON to send: {}", command_str);

                            // Send command directly without helper function to avoid any scope issues
                            println!("DEBUG: Writing command to stream...");
                            if let Err(e) = stream.write_all(command_str.as_bytes()) {
                                eprintln!("Failed to write command: {}", e);
                                vm_result.error = Some(format!("Write failed: {}", e));
                            } else {
                                println!("DEBUG: Command written, flushing...");
                                if let Err(e) = stream.flush() {
                                    eprintln!("Failed to flush: {}", e);
                                    vm_result.error = Some(format!("Flush failed: {}", e));
                                } else {
                                    println!("DEBUG: Command sent, reading response...");

                                    // Read response immediately
                                    let mut response_buffer = Vec::new();
                                    let mut temp_buffer = [0u8; 1024];
                                    let mut attempts = 0;
                                    let max_attempts = 50; // 5 seconds total

                                    while attempts < max_attempts {
                                        match stream.read(&mut temp_buffer) {
                                            Ok(0) => {
                                                println!(
                                                    "Connection closed by agent after {} bytes",
                                                    response_buffer.len()
                                                );
                                                break;
                                            }
                                            Ok(n) => {
                                                response_buffer
                                                    .extend_from_slice(&temp_buffer[..n]);
                                                println!(
                                                    "Read {} bytes (total: {})",
                                                    n,
                                                    response_buffer.len()
                                                );

                                                // Check if we have a complete JSON response
                                                if let Ok(response_str) =
                                                    String::from_utf8(response_buffer.clone())
                                                {
                                                    if let Ok(_) =
                                                        serde_json::from_str::<serde_json::Value>(
                                                            &response_str,
                                                        )
                                                    {
                                                        println!("Complete JSON response received");
                                                        break;
                                                    }
                                                }
                                            }
                                            Err(ref e)
                                                if e.kind() == std::io::ErrorKind::WouldBlock =>
                                            {
                                                // Wait a bit and try again
                                                std::thread::sleep(Duration::from_millis(100));
                                                attempts += 1;
                                                continue;
                                            }
                                            Err(e) => {
                                                println!("Read error: {}", e);
                                                break;
                                            }
                                        }
                                    }

                                    // Process the response
                                    if !response_buffer.is_empty() {
                                        if let Ok(response_str) = String::from_utf8(response_buffer)
                                        {
                                            println!("Response: {}", response_str);

                                            if let Ok(response_json) =
                                                serde_json::from_str::<serde_json::Value>(
                                                    &response_str,
                                                )
                                            {
                                                // Parse response
                                                if let Some(exit_code) = response_json
                                                    .get("exit_code")
                                                    .and_then(|v| v.as_i64())
                                                {
                                                    vm_result.exit_code = exit_code as i32;
                                                }
                                                if let Some(stdout) = response_json
                                                    .get("stdout")
                                                    .and_then(|v| v.as_str())
                                                {
                                                    vm_result.stdout = stdout.to_string();
                                                }
                                                if let Some(stderr) = response_json
                                                    .get("stderr")
                                                    .and_then(|v| v.as_str())
                                                {
                                                    vm_result.stderr = stderr.to_string();
                                                }
                                                vm_result.error = None;
                                                println!(
                                                    "Successfully processed response: exit_code={}",
                                                    vm_result.exit_code
                                                );
                                            } else {
                                                println!("Failed to parse JSON response");
                                                vm_result.error =
                                                    Some("Invalid JSON response".to_string());
                                            }
                                        } else {
                                            println!("Invalid UTF-8 in response");
                                            vm_result.error =
                                                Some("Invalid UTF-8 response".to_string());
                                        }
                                    } else {
                                        println!("No response received");
                                        vm_result.error = Some("No response received".to_string());
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Failed to connect to VM {} via Unix socket {}: {}",
                            vm_id, vsock_socket_path, e
                        );
                        vm_result.error = Some(format!("Connection failed: {}", e));
                    }
                }

                // Always send the result back (success or failure)
                if let Some(sender) = result_sender {
                    println!("Sending command result back: exit_code={}, stdout_len={}, stderr_len={}, error={:?}",
                             vm_result.exit_code, vm_result.stdout.len(), vm_result.stderr.len(), vm_result.error);
                    if let Err(e) = sender.send(vm_result) {
                        eprintln!("Failed to send command result back: {}", e);
                    } else {
                        println!("Command result sent successfully");
                    }
                } else {
                    eprintln!("No result sender found for command {}", command.id);
                }
            }
        });
    }

    pub async fn execute_command_in_vm(
        &self,
        vm_id: &str,
        command: String,
        args: Vec<String>,
        working_dir: Option<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let cmd_id = format!("cmd_{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));

        // Get the VM instance and create result channel
        let (command_sender, result_receiver) = {
            let instances = self.instances.lock().unwrap();
            if let Some(vm_instance) = instances.get(vm_id) {
                // Create a channel to receive the result for this specific command
                let (result_sender, result_receiver) = mpsc::channel::<VmCommandResult>();

                // Store the result sender in the VM instance
                {
                    let mut result_receivers = vm_instance.result_receiver.lock().unwrap();
                    result_receivers.insert(cmd_id.clone(), result_sender);
                }

                (vm_instance.command_sender.clone(), result_receiver)
            } else {
                return Err(format!("VM {} not found", vm_id).into());
            }
        };

        let vm_command = VmCommand {
            id: cmd_id.clone(),
            command: command.clone(),
            args: args.clone(),
            working_dir: working_dir.clone(),
            timeout_seconds,
        };

        // Send command to VM
        if let Err(e) = command_sender.send(vm_command) {
            return Err(format!(
                "Failed to send command to VM: sending on a closed channel - {}",
                e
            )
            .into());
        }

        println!("DEBUG: Starting to wait for result for command {}", cmd_id);

        // Wait for the result with timeout - increase for commands known to produce large outputs
        let default_timeout = if command == "dmesg"
            || command.contains("journalctl")
            || command.contains("cat /var/log")
            || command.starts_with("find /")
            || command.contains("grep -r")
        {
            300 // 5 minutes for large output commands
        } else {
            30 // 30 seconds for regular commands
        };
        let timeout_duration = Duration::from_secs(timeout_seconds.unwrap_or(default_timeout));
        let start_time = Instant::now();

        loop {
            match result_receiver.try_recv() {
                Ok(result) => {
                    println!(
                        "DEBUG: Received result for command {}: exit_code={}",
                        cmd_id, result.exit_code
                    );
                    // Clean up the result receiver
                    {
                        let instances = self.instances.lock().unwrap();
                        if let Some(vm_instance) = instances.get(vm_id) {
                            let mut result_receivers = vm_instance.result_receiver.lock().unwrap();
                            result_receivers.remove(&cmd_id);
                        }
                    }

                    // Return the actual command output
                    if result.exit_code == 0 {
                        println!("DEBUG: Returning successful result for command {}", cmd_id);
                        return Ok(result.stdout);
                    } else {
                        return Err(format!(
                            "Command failed with exit code {}: {}",
                            result.exit_code, result.stderr
                        )
                        .into());
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    if start_time.elapsed() > timeout_duration {
                        println!(
                            "DEBUG: Timeout waiting for result for command {} after {:?}",
                            cmd_id,
                            start_time.elapsed()
                        );
                        // Clean up on timeout
                        {
                            let instances = self.instances.lock().unwrap();
                            if let Some(vm_instance) = instances.get(vm_id) {
                                let mut result_receivers =
                                    vm_instance.result_receiver.lock().unwrap();
                                println!(
                                    "DEBUG: Removing result receiver for command {} due to timeout",
                                    cmd_id
                                );
                                result_receivers.remove(&cmd_id);
                            }
                        }
                        return Err("Command execution timed out".into());
                    }
                    // Small sleep to prevent busy waiting
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    println!("DEBUG: Result receiver disconnected for command {}", cmd_id);
                    return Err("VM disconnected while waiting for command result".into());
                }
            }
        }
    }

    /// Check if VM channels are healthy by sending a health check command
    pub async fn check_vm_health(&self, vm_id: &str) -> bool {
        let instances = self.instances.lock().unwrap();
        if let Some(vm_instance) = instances.get(vm_id) {
            let health_check_cmd = VmCommand {
                id: "health_check".to_string(),
                command: "echo".to_string(),
                args: vec!["test".to_string()],
                working_dir: None,
                timeout_seconds: Some(5),
            };

            // Try to send a health check command
            vm_instance.command_sender.send(health_check_cmd).is_ok()
        } else {
            false
        }
    }

    /// Reconnect VM channels when they're closed
    pub async fn reconnect_vm_channels(
        &self,
        vm_id: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("DEBUG: Attempting to reconnect channels for VM {}", vm_id);

        // Check if VM is still healthy
        if self.check_vm_health(vm_id).await {
            println!(
                "DEBUG: VM {} channels are healthy, no reconnection needed",
                vm_id
            );
            return Ok(());
        }

        println!("DEBUG: VM {} channels are unhealthy, recreating VM", vm_id);

        // Store the current VM's CID before destroying it
        let _cid = {
            let instances = self.instances.lock().unwrap();
            instances.get(vm_id).map(|vm| vm.cid)
        };

        // Destroy the existing VM
        if let Err(e) = self.destroy_vm(vm_id).await {
            eprintln!("Warning: Failed to destroy VM during reconnection: {}", e);
        }

        // Recreate the VM
        match self.create_vm(vm_id.to_string()).await {
            Ok(_) => {
                println!("DEBUG: Successfully reconnected VM {}", vm_id);
                Ok(())
            }
            Err(e) => {
                eprintln!("ERROR: Failed to recreate VM {}: {}", vm_id, e);
                Err(e)
            }
        }
    }

    /// Execute command with automatic retry and channel recovery
    pub async fn execute_command_with_retry(
        &self,
        vm_id: &str,
        command: String,
        args: Vec<String>,
        working_dir: Option<String>,
        timeout_seconds: Option<u64>,
        max_retries: u32,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut retries = 0;

        loop {
            match self
                .execute_command_in_vm(
                    vm_id,
                    command.clone(),
                    args.clone(),
                    working_dir.clone(),
                    timeout_seconds,
                )
                .await
            {
                Ok(result) => return Ok(result),
                Err(e) => {
                    let error_msg = e.to_string();

                    // Check if it's a channel-related error
                    if (error_msg.contains("sending on a closed channel")
                        || error_msg.contains("VM disconnected")
                        || error_msg.contains("Failed to send command to VM"))
                        && retries < max_retries
                    {
                        println!("DEBUG: Channel error detected, attempting to reconnect and retry (attempt {}/{})", retries + 1, max_retries);

                        // Try to reconnect the VM channels
                        if let Err(reconnect_err) = self.reconnect_vm_channels(vm_id).await {
                            eprintln!("Failed to reconnect VM channels: {}", reconnect_err);
                        }

                        // Exponential backoff
                        let delay = std::time::Duration::from_millis(1000 * (2_u64.pow(retries)));
                        tokio::time::sleep(delay).await;

                        retries += 1;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Public wrapper that uses retry logic by default for VM command execution
    pub async fn execute_vm_command(
        &self,
        vm_id: &str,
        command: String,
        args: Vec<String>,
        working_dir: Option<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // Use retry logic by default with 3 attempts
        self.execute_command_with_retry(
            vm_id,
            command,
            args,
            working_dir,
            timeout_seconds,
            3, // max_retries
        )
        .await
    }

    // pub async fn execute_command_in_vm_with_fallback(
    //     &self,
    //     vm_id: &str,
    //     command: String,
    //     args: Vec<String>,
    //     working_dir: Option<String>,
    //     timeout_seconds: Option<u64>,
    // ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    //     // First try the real VSOCK connection
    //     match self
    //         .execute_command_in_vm(
    //             vm_id,
    //             command.clone(),
    //             args.clone(),
    //             working_dir.clone(),
    //             timeout_seconds,
    //         )
    //         .await
    //     {
    //         Ok(result) => Ok(result),
    //         Err(e) => {
    //             // If VSOCK fails, provide a simulation with diagnostic info
    //             let diagnostic = self
    //                 .diagnose_vm_boot_issues(vm_id)
    //                 .unwrap_or_else(|_| "VM diagnostics unavailable".to_string());

    //             let simulated_result = if command == "echo" && !args.is_empty() {
    //                 args.join(" ")
    //             } else if command == "uname" {
    //                 "Linux vm-guest 5.15.0 #1 SMP PREEMPT x86_64 GNU/Linux".to_string()
    //             } else if command == "whoami" {
    //                 "root".to_string()
    //             } else if command == "pwd" {
    //                 working_dir.unwrap_or("/".to_string())
    //             } else if command == "ls" {
    //                 "bin  etc  proc  sys  tmp  usr  var".to_string()
    //             } else if command == "dmesg" {
    //                 "[    0.000000] Linux version 5.15.0\n[    0.001000] Command line: console=ttyS0\n[    1.234567] VSOCK initialized".to_string()
    //             } else {
    //                 format!("Command '{}' output (simulated)", command)
    //             };

    //             Ok(format!(
    //                 "SIMULATED RESULT (VSOCK failed: {}):\n{}\n\nDIAGNOSTICS:\n{}",
    //                 e, simulated_result, diagnostic
    //             ))
    //         }
    //     }
    //}

    pub fn list_vms(&self) -> Vec<String> {
        let instances = self.instances.lock().unwrap();
        instances.keys().cloned().collect()
    }

    pub async fn destroy_vm(
        &self,
        vm_id: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let mut instances = self.instances.lock().unwrap();

        if let Some(vm_instance) = instances.remove(vm_id) {
            // Kill the VM process if it exists
            if let Some(pid) = vm_instance.pid {
                let _ = Command::new("kill")
                    .args(&["-9", &pid.to_string()])
                    .output();
            }

            // The temporary directory will be automatically cleaned up when the TempDir is dropped
            Ok(format!("VM {} destroyed successfully", vm_id))
        } else {
            Err(format!("VM {} not found", vm_id).into())
        }
    }

    // pub fn check_vm_health(
    //     &self,
    //     vm_id: &str,
    // ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    //     let instances = self.instances.lock().unwrap();
    //     if let Some(vm_instance) = instances.get(vm_id) {
    //         let mut health_report = String::new();

    //         health_report.push_str(&format!("VM {} Health Check:\n", vm_id));
    //         health_report.push_str(&format!("  CID: {}\n", vm_instance.cid));
    //         health_report.push_str(&format!("  PID: {:?}\n", vm_instance.pid));

    //         // Check if VM process is still running
    //         if let Some(pid) = vm_instance.pid {
    //             let process_running = std::process::Command::new("kill")
    //                 .args(&["-0", &pid.to_string()])
    //                 .output()
    //                 .map(|output| output.status.success())
    //                 .unwrap_or(false);

    //             health_report.push_str(&format!("  Process running: {}\n", process_running));
    //         }

    //         // Check if VSOCK socket exists
    //         let vm_dir = vm_instance.temp_dir.path();
    //         let vsock_path = vm_dir.join("vsock.sock");
    //         let vsock_exists = vsock_path.exists();
    //         health_report.push_str(&format!("  VSOCK socket exists: {}\n", vsock_exists));

    //         if vsock_exists {
    //             health_report.push_str(&format!("  VSOCK socket path: {}\n", vsock_path.display()));
    //         }

    //         // Try to connect to VSOCK
    //         match VsockStream::connect_with_cid_port(vm_instance.cid, 1234) {
    //             Ok(_) => {
    //                 health_report.push_str("  VSOCK connection: SUCCESS\n");
    //             }
    //             Err(e) => {
    //                 health_report.push_str(&format!("  VSOCK connection: FAILED ({})\n", e));
    //             }
    //         }

    //         Ok(health_report)
    //     } else {
    //         Err(format!("VM {} not found", vm_id).into())
    //     }
    // }

    // pub fn diagnose_vm_boot_issues(
    //     &self,
    //     vm_id: &str,
    // ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    //     let instances = self.instances.lock().unwrap();
    //     if let Some(vm_instance) = instances.get(vm_id) {
    //         let mut report = String::new();

    //         report.push_str(&format!("=== VM Boot Diagnostics for {} ===\n", vm_id));

    //         // Check VM files
    //         let vm_dir = vm_instance.temp_dir.path();
    //         let kernel_path = vm_dir.join("vmlinux");
    //         let rootfs_path = vm_dir.join("rootfs.ext4");
    //         let config_path = vm_dir.join("firecracker-config.json");
    //         let vsock_path = vm_dir.join("vsock.sock");

    //         report.push_str(&format!("VM Directory: {}\n", vm_dir.display()));
    //         report.push_str(&format!(
    //             "Kernel exists: {} ({})\n",
    //             kernel_path.exists(),
    //             if kernel_path.exists() {
    //                 format!(
    //                     "{} bytes",
    //                     std::fs::metadata(&kernel_path)
    //                         .map(|m| m.len())
    //                         .unwrap_or(0)
    //                 )
    //             } else {
    //                 "N/A".to_string()
    //             }
    //         ));
    //         report.push_str(&format!(
    //             "Rootfs exists: {} ({})\n",
    //             rootfs_path.exists(),
    //             if rootfs_path.exists() {
    //                 format!(
    //                     "{} bytes",
    //                     std::fs::metadata(&rootfs_path)
    //                         .map(|m| m.len())
    //                         .unwrap_or(0)
    //                 )
    //             } else {
    //                 "N/A".to_string()
    //             }
    //         ));
    //         report.push_str(&format!("Config exists: {}\n", config_path.exists()));
    //         report.push_str(&format!("VSOCK socket exists: {}\n", vsock_path.exists()));

    //         // Check if files are real or placeholders
    //         if kernel_path.exists() {
    //             if let Ok(content) = std::fs::read_to_string(&kernel_path) {
    //                 if content.starts_with("placeholder") {
    //                     report.push_str(
    //                         "WARNING: Kernel is a placeholder file, not a real kernel!\n",
    //                     );
    //                 }
    //             }
    //         }

    //         // Check process status
    //         if let Some(pid) = vm_instance.pid {
    //             let process_running = std::process::Command::new("kill")
    //                 .args(&["-0", &pid.to_string()])
    //                 .output()
    //                 .map(|output| output.status.success())
    //                 .unwrap_or(false);

    //             report.push_str(&format!(
    //                 "VM Process (PID {}): {}\n",
    //                 pid,
    //                 if process_running {
    //                     "RUNNING"
    //                 } else {
    //                     "STOPPED"
    //                 }
    //             ));

    //             // Try to get process info
    //             if let Ok(output) = std::process::Command::new("ps")
    //                 .args(&["-p", &pid.to_string(), "-o", "pid,ppid,cmd"])
    //                 .output()
    //             {
    //                 if output.status.success() {
    //                     report.push_str(&format!(
    //                         "Process info:\n{}\n",
    //                         String::from_utf8_lossy(&output.stdout)
    //                     ));
    //                 }
    //             }
    //         }

    //         // Check VSOCK connectivity
    //         report.push_str(&format!("VSOCK Test (CID {}): ", vm_instance.cid));
    //         match VsockStream::connect_with_cid_port(vm_instance.cid, 1234) {
    //             Ok(_) => report.push_str("SUCCESS\n"),
    //             Err(e) => report.push_str(&format!("FAILED - {}\n", e)),
    //         }

    //         // Check system VSOCK support
    //         report.push_str("\n=== System VSOCK Status ===\n");
    //         if let Ok(output) = std::process::Command::new("lsmod").output() {
    //             let lsmod_output = String::from_utf8_lossy(&output.stdout);
    //             if lsmod_output.contains("vsock") {
    //                 report.push_str("VSOCK kernel modules: LOADED\n");
    //             } else {
    //                 report.push_str("VSOCK kernel modules: NOT LOADED\n");
    //             }
    //         }

    //         report.push_str(&format!(
    //             "VSOCK device: {}\n",
    //             if std::path::Path::new("/dev/vsock").exists() {
    //                 "EXISTS"
    //             } else {
    //                 "MISSING"
    //             }
    //         ));

    //         Ok(report)
    //     } else {
    //         Err(format!("VM {} not found", vm_id).into())
    //     }
    //  }

    // Helper function to send command and read response atomically
    // fn send_command_and_read_response(
    //     stream: &mut UnixStream,
    //     command_str: &str,
    // ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    //     println!("DEBUG: Writing command to stream...");

    //     // Send the command
    //     stream.write_all(command_str.as_bytes())?;
    //     println!("DEBUG: Command written, flushing...");

    //     stream.flush()?;
    //     println!("DEBUG: Command flushed, starting to read response...");

    //     // Read the response
    //     let mut response_buffer = Vec::new();
    //     let mut temp_buffer = [0u8; 4096];
    //     let response_timeout = Duration::from_secs(15);
    //     let start_time = Instant::now();

    //     loop {
    //         println!(
    //             "DEBUG: Attempting to read from stream (total so far: {} bytes)...",
    //             response_buffer.len()
    //         );

    //         match stream.read(&mut temp_buffer) {
    //             Ok(0) => {
    //                 // Connection closed by agent - this is normal after sending response
    //                 println!(
    //                     "Connection closed by agent after reading {} bytes",
    //                     response_buffer.len()
    //                 );
    //                 break;
    //             }
    //             Ok(n) => {
    //                 response_buffer.extend_from_slice(&temp_buffer[..n]);
    //                 println!(
    //                     "Read {} bytes from VM (total: {})",
    //                     n,
    //                     response_buffer.len()
    //                 );

    //                 // Try to parse JSON to see if we have a complete response
    //                 if let Ok(response_str) = String::from_utf8(response_buffer.clone()) {
    //                     if let Ok(_) = serde_json::from_str::<serde_json::Value>(&response_str) {
    //                         println!("Complete JSON response received, breaking read loop");
    //                         break;
    //                     }
    //                 }
    //                 // Continue reading if JSON is not yet complete
    //             }
    //             Err(ref e)
    //                 if e.kind() == std::io::ErrorKind::WouldBlock
    //                     || e.kind() == std::io::ErrorKind::TimedOut =>
    //             {
    //                 println!("DEBUG: WouldBlock/TimedOut, sleeping 50ms...");
    //                 std::thread::sleep(Duration::from_millis(50));
    //                 continue;
    //             }
    //             Err(e) => {
    //                 println!("Read error: {}", e);
    //                 // If we have some data, try to use it
    //                 if !response_buffer.is_empty() {
    //                     println!(
    //                         "Got read error but have {} bytes, using what we have",
    //                         response_buffer.len()
    //                     );
    //                     break;
    //                 } else {
    //                     return Err(format!("Read error with no data: {}", e).into());
    //                 }
    //             }
    //         }

    //         // Check overall timeout
    //         if start_time.elapsed() > response_timeout {
    //             println!(
    //                 "Overall response timeout reached after {:?}",
    //                 start_time.elapsed()
    //             );
    //             if response_buffer.is_empty() {
    //                 return Err("No response received within timeout".into());
    //             } else {
    //                 println!(
    //                     "Timeout but have {} bytes, using what we have",
    //                     response_buffer.len()
    //                 );
    //                 break;
    //             }
    //         }
    //     }

    //     println!(
    //         "DEBUG: Finished reading response, got {} bytes total",
    //         response_buffer.len()
    //     );
    //     Ok(response_buffer)
    // }

    // Add shutdown method to cleanly terminate all VMs
    pub fn shutdown(&self) {
        println!("Shutting down VM Manager...");
        self.shutting_down.store(true, Ordering::SeqCst);

        // Get all VM instances PIDs and IDs
        let vm_pids: Vec<(String, Option<u32>)> = {
            let instances_guard = self.instances.lock().unwrap();
            instances_guard
                .iter()
                .map(|(id, instance)| (id.clone(), instance.pid))
                .collect()
        };

        if vm_pids.is_empty() {
            println!("No VM instances to clean up");
            return;
        }

        println!("Cleaning up {} VM instance(s)...", vm_pids.len());

        for (vm_id, pid_opt) in vm_pids {
            if let Some(pid) = pid_opt {
                println!("Terminating Firecracker VM {} (PID: {})", vm_id, pid);

                // First try SIGTERM (graceful shutdown)
                if let Err(e) = Self::terminate_process(pid, "TERM") {
                    println!("Failed to send SIGTERM to PID {}: {}", pid, e);

                    // If SIGTERM fails, try SIGKILL (force kill)
                    if let Err(e) = Self::terminate_process(pid, "KILL") {
                        eprintln!("Failed to kill PID {}: {}", pid, e);
                    } else {
                        println!("Force killed PID {}", pid);
                    }
                } else {
                    // Wait a moment to see if process terminates gracefully
                    std::thread::sleep(Duration::from_millis(500));

                    // Check if process is still running
                    if Self::is_process_running(pid) {
                        println!(
                            "Process {} didn't terminate gracefully, force killing...",
                            pid
                        );
                        if let Err(e) = Self::terminate_process(pid, "KILL") {
                            eprintln!("Failed to force kill PID {}: {}", pid, e);
                        } else {
                            println!("Force killed PID {}", pid);
                        }
                    } else {
                        println!("Process {} terminated gracefully", pid);
                    }
                }
            } else {
                println!("VM {} has no PID recorded (may be simulated)", vm_id);
            }
        }

        // Clear all instances
        {
            let mut instances_guard = self.instances.lock().unwrap();
            instances_guard.clear();
        }

        // Close VSOCK listener
        {
            let mut listener_guard = self.vsock_listener.lock().unwrap();
            *listener_guard = None;
        }

        println!("VM Manager shutdown complete");
    }

    // Helper function to terminate a process
    fn terminate_process(pid: u32, signal: &str) -> Result<(), std::io::Error> {
        Command::new("kill")
            .arg(format!("-{}", signal))
            .arg(pid.to_string())
            .output()
            .map(|_| ())
    }

    // Helper function to check if a process is still running
    fn is_process_running(pid: u32) -> bool {
        Command::new("kill")
            .arg("-0") // Signal 0 just checks if process exists
            .arg(pid.to_string())
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    // Add cleanup for all running firecracker processes (emergency cleanup)
    pub fn emergency_cleanup() {
        println!("Performing emergency Firecracker cleanup...");

        // Find all firecracker processes
        let output = Command::new("pgrep").arg("-f").arg("firecracker").output();

        match output {
            Ok(output) if output.status.success() => {
                let pids_str = String::from_utf8_lossy(&output.stdout);
                let pids: Vec<u32> = pids_str
                    .lines()
                    .filter_map(|line| line.trim().parse().ok())
                    .collect();

                if pids.is_empty() {
                    println!("No Firecracker processes found");
                    return;
                }

                println!("Found {} Firecracker process(es): {:?}", pids.len(), pids);

                for pid in pids {
                    println!("Killing Firecracker process {}", pid);
                    if let Err(e) = Self::terminate_process(pid, "KILL") {
                        eprintln!("Failed to kill Firecracker PID {}: {}", pid, e);
                    }
                }
            }
            Ok(_) => {
                println!("No Firecracker processes found (pgrep returned non-zero)");
            }
            Err(e) => {
                eprintln!("Failed to search for Firecracker processes: {}", e);
            }
        }
    }

    // Add a method to check if we're shutting down
    // pub fn is_shutting_down(&self) -> bool {
    //     self.shutting_down.load(Ordering::SeqCst)
    // }
}

// Implement Drop trait for VmManager as a fallback cleanup
impl Drop for VmManager {
    fn drop(&mut self) {
        if !self.shutting_down.load(Ordering::SeqCst) {
            println!("VmManager dropped without explicit shutdown, cleaning up...");
            self.shutdown();
        }
    }
}
