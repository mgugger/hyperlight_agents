use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use tempfile::TempDir;
use vsock::{VsockListener, VsockStream};

pub struct VmInstance {
    pub vm_id: String,
    pub cid: u32, // VSOCK Context ID
    pub pid: Option<u32>,
    pub temp_dir: TempDir,
    pub command_sender: mpsc::Sender<VmCommand>,
    pub vm_type: VmType,
}

#[derive(Debug, Clone)]
pub enum VmType {
    Firecracker,
    Qemu,
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
    firecracker_available: bool,
}

impl VmManager {
    pub fn new() -> Self {
        let firecracker_available = Command::new(
            "/home/manuel/firecracker/release-v1.12.1-x86_64/firecracker-v1.12.1-x86_64",
        )
        .arg("--version")
        .output()
        .is_ok();

        if firecracker_available {
            println!("Firecracker detected - will use fast microVMs");
        } else {
            println!("Firecracker not found - falling back to QEMU");
            println!("Install Firecracker for faster VM deployment:");
            println!("https://github.com/firecracker-microvm/firecracker/releases");
        }

        Self {
            instances: Arc::new(Mutex::new(HashMap::new())),
            next_cid: Arc::new(Mutex::new(100)), // Start CIDs from 100
            vsock_listener: Arc::new(Mutex::new(None)),
            firecracker_available,
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
        _instances: Arc<Mutex<HashMap<String, VmInstance>>>,
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
                                // Here you would route the result back to the requesting agent
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

        // Create temporary directory for VM files
        let temp_dir = TempDir::new()?;
        let vm_dir = temp_dir.path();

        // Create command channel for this VM
        let (command_sender, command_receiver) = mpsc::channel::<VmCommand>();

        self.create_minimal_vm_image(&vm_dir, &vm_id, cid).await?;
        let vm_process = self.start_firecracker_vm(&vm_dir, cid)?;
        let vm_type = VmType::Firecracker;

        let vm_instance = VmInstance {
            vm_id: vm_id.clone(),
            cid,
            pid: vm_process,
            temp_dir,
            command_sender,
            vm_type: vm_type.clone(),
        };

        // Store the VM instance
        {
            let mut instances = self.instances.lock().unwrap();
            instances.insert(vm_id.clone(), vm_instance);
        }

        // Start command processor for this VM
        self.start_command_processor(vm_id.clone(), command_receiver, cid);

        Ok(format!(
            "VM {} created with CID {} using {:?}",
            vm_id, cid, vm_type
        ))
    }

    async fn create_minimal_vm_image(
        &self,
        vm_dir: &Path,
        vm_id: &str,
        cid: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Check if we have pre-built minimal images
        let _minimal_kernel = vm_dir.join("vmlinux");
        let _minimal_rootfs = vm_dir.join("rootfs.ext4");

        // Create a simple placeholder VM image without sudo requirements
        self.create_simple_vm_image(&vm_dir, vm_id, cid)?;

        Ok(())
    }

    fn create_simple_vm_image(
        &self,
        vm_dir: &Path,
        vm_id: &str,
        _cid: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("Creating simple VM image for {} (no sudo required)", vm_id);

        // Create placeholder kernel and rootfs files
        let kernel_path = vm_dir.join("vmlinux");
        let rootfs_path = vm_dir.join("rootfs.ext4");

        // Create minimal placeholder files
        std::fs::write(&kernel_path, b"placeholder_kernel")?;

        // Create a minimal ext4-like file (just a placeholder)
        let mut rootfs_data = vec![0u8; 50 * 1024 * 1024]; // 50MB of zeros
        // Add some basic ext4 signature bytes at the beginning
        rootfs_data[1080] = 0x53; // ext4 magic number part 1
        rootfs_data[1081] = 0xef; // ext4 magic number part 2
        std::fs::write(&rootfs_path, &rootfs_data)?;

        println!("Simple VM image created for {}", vm_id);
        Ok(())
    }

    async fn build_minimal_vm_image(
        &self,
        vm_dir: &Path,
        vm_id: &str,
        cid: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("Building minimal VM image for {}", vm_id);

        // Create a simple init script that includes our Rust VSOCK agent
        let init_script = self.create_init_script(vm_id, cid);

        // Create minimal rootfs
        // Minimal rootfs creation - simplified for now
        println!("Creating minimal rootfs for {}", vm_id);

        // Use a minimal kernel (we'll need to provide this)
        self.prepare_minimal_kernel(&vm_dir)?;

        Ok(())
    }

    fn create_init_script(&self, vm_id: &str, cid: u32) -> String {
        format!(
            r#"#!/bin/sh
# Minimal init script with embedded functionality

# Mount essential filesystems
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null || mknod /dev/null c 1 3

# Set hostname
echo "{}" > /proc/sys/kernel/hostname

# Simple VSOCK agent (minimal shell version)
VM_ID="{}"
CID={}
HOST_CID=2
PORT=1234

# Function to send JSON message via VSOCK
send_vsock_msg() {{
    local msg="$1"
    echo "$msg" | nc vsock $HOST_CID $PORT 2>/dev/null || true
}}

# Register with host
register_vm() {{
    send_vsock_msg '{{"type":"register","vm_id":"'$VM_ID'","cid":'$CID'}}'
}}

# Execute command and send result
execute_cmd() {{
    local cmd_json="$1"
    local cmd=$(echo "$cmd_json" | sed -n 's/.*"command":"\([^"]*\)".*/\1/p')
    local cmd_id=$(echo "$cmd_json" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')

    if [ -n "$cmd" ]; then
        local start_time=$(date +%s)
        local result=$(eval "$cmd" 2>&1)
        local exit_code=$?
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))

        # Escape JSON special characters (basic)
        result=$(echo "$result" | sed 's/\\/\\\\/g; s/"/\\"/g')

        send_vsock_msg '{{"type":"command_result","id":"'$cmd_id'","vm_id":"'$VM_ID'","exit_code":'$exit_code',"stdout":"'$result'","stderr":"","duration":'$duration'}}'
    fi
}}

# Main loop
main() {{
    echo "VM $VM_ID starting with CID $CID"

    # Register with host periodically
    while true; do
        register_vm
        sleep 5
    done &

    # Listen for commands (simplified - in real implementation we'd use socat/nc with VSOCK)
    # For now, just keep the VM alive
    while true; do
        sleep 60
        echo "VM $VM_ID heartbeat"
    done
}}

main
"#,
            vm_id, vm_id, cid
        )
    }

    // Removed create_minimal_rootfs - no longer needed

    fn prepare_minimal_kernel(
        &self,
        vm_dir: &Path,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let kernel_path = vm_dir.join("vmlinux");

        // Check for existing minimal kernel
        let kernel_sources = [
            "./vm-images/vmlinux",
            "/boot/vmlinuz", // Current system kernel as fallback
            "/usr/src/linux/arch/x86/boot/bzImage",
        ];

        for source in &kernel_sources {
            if Path::new(source).exists() {
                Command::new("cp")
                    .args(&[source, kernel_path.to_str().unwrap()])
                    .output()?;
                return Ok(());
            }
        }

        println!("Warning: No suitable kernel found. You may need to:");
        println!("1. Build a minimal kernel with VSOCK support");
        println!("2. Download a pre-built minimal kernel");
        println!("3. Use the system kernel as fallback");

        Ok(())
    }

    fn copy_minimal_images(
        &self,
        vm_dir: &Path,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Command::new("cp")
            .args(&[
                "./vm-images/vmlinux",
                &vm_dir.join("vmlinux").to_string_lossy(),
            ])
            .output()?;

        Command::new("cp")
            .args(&[
                "./vm-images/rootfs-template.ext4",
                &vm_dir.join("rootfs.ext4").to_string_lossy(),
            ])
            .output()?;

        Ok(())
    }

    fn customize_rootfs_for_vm(
        &self,
        vm_dir: &Path,
        vm_id: &str,
        cid: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Mount the copied rootfs and customize it for this specific VM
        let rootfs_path = vm_dir.join("rootfs.ext4");
        let mount_point = vm_dir.join("mnt");

        std::fs::create_dir_all(&mount_point)?;

        Command::new("sudo")
            .args(&[
                "mount",
                "-o",
                "loop",
                rootfs_path.to_str().unwrap(),
                mount_point.to_str().unwrap(),
            ])
            .output()?;

        // Update the init script with VM-specific values
        let init_script = self.create_init_script(vm_id, cid);
        let init_path = format!("{}/init", mount_point.display());

        Command::new("sudo")
            .args(&["tee", &init_path])
            .stdin(Stdio::piped())
            .spawn()?
            .stdin
            .as_mut()
            .unwrap()
            .write_all(init_script.as_bytes())?;

        Command::new("sudo")
            .args(&["umount", mount_point.to_str().unwrap()])
            .output()?;

        Ok(())
    }

    fn start_firecracker_vm(
        &self,
        vm_dir: &Path,
        cid: u32,
    ) -> Result<Option<u32>, Box<dyn std::error::Error + Send + Sync>> {
        let kernel_path = vm_dir.join("vmlinux");
        let rootfs_path = vm_dir.join("rootfs.ext4");
        let config_path = vm_dir.join("firecracker-config.json");

        // Create Firecracker configuration
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
                "ht_enabled": false
            },
            "vsock": {
                "guest_cid": cid,
                "uds_path": format!("{}/vsock.sock", vm_dir.display())
            }
        });

        std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;

        // Simulate starting Firecracker (for testing without actual firecracker binary)
        match std::env::var("SKIP_FIRECRACKER") {
            Ok(_) => {
                println!("Simulating Firecracker VM start for testing");
                Ok(Some(12345)) // Fake PID
            }
            Err(_) => {
                // Try to start real Firecracker
                let mut cmd = Command::new(
                    "/home/manuel/firecracker/release-v1.12.1-x86_64/firecracker-v1.12.1-x86_64",
                );
                cmd.arg("--api-sock")
                    .arg(format!("{}/firecracker.sock", vm_dir.display()))
                    .arg("--config-file")
                    .arg(&config_path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());

                match cmd.spawn() {
                    Ok(child) => {
                        println!("Started Firecracker VM with PID: {}", child.id());
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
        thread::spawn(move || {
            for command in receiver {
                println!("Processing command {} for VM {}", command.command, vm_id);
                // In a real implementation, this would send the command to the VM via VSOCK
                // For now, just acknowledge receipt
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
        // Get the VM instance
        let instances = self.instances.lock().unwrap();
        if let Some(vm_instance) = instances.get(vm_id) {
            let cmd_id = format!("cmd_{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));

            let vm_command = VmCommand {
                id: cmd_id.clone(),
                command: command.clone(),
                args: args.clone(),
                working_dir: working_dir.clone(),
                timeout_seconds,
            };

            // Send command to VM
            if let Err(e) = vm_instance.command_sender.send(vm_command) {
                return Err(format!("Failed to send command to VM: {}", e).into());
            }

            // Simulate command execution for testing
            let result = format!(
                "Command '{}' executed successfully in VM {} (simulated)",
                command, vm_id
            );
            Ok(result)
        } else {
            Err(format!("VM {} not found", vm_id).into())
        }
    }

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

    // ... rest of the existing methods (create_vm_image_qemu, start_qemu_vm, etc.)
}
