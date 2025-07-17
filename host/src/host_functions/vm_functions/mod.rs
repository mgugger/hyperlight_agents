pub mod firecracker;
pub mod http_proxy;
pub mod log_listener;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use vsock::{VsockListener, VsockStream};

// Structs used across the module
pub struct VmInstance {
    pub vm_id: String,
    pub cid: u32,
    pub pid: Option<u32>,
    pub temp_dir: TempDir,
    pub command_sender: mpsc::Sender<VmCommand>,
    pub result_receiver: Arc<Mutex<HashMap<String, mpsc::Sender<VmCommandResult>>>>,
    pub memfd_rootfs: Option<memfd::Memfd>,
    pub rootfs_symlink: Option<PathBuf>,
}

use hyperlight_agents_common::{VmCommand, VmCommandMode, VmCommandResult};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum VsockRequest {
    Command(VmCommand),
    HttpProxy(http_proxy::HttpProxyRequest),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum VsockResponse {
    Command(serde_json::Value),
    HttpProxy(http_proxy::HttpProxyResponse),
}

// The main VmManager struct
pub struct VmManager {
    pub(crate) instances: Arc<Mutex<HashMap<String, VmInstance>>>,
    pub(crate) next_cid: Arc<Mutex<u32>>,
    pub(crate) shutdown_flag: Arc<AtomicBool>,
    vsock_listener: Arc<Mutex<Option<VsockListener>>>,
    pub(crate) http_client: Arc<Client>,
}

impl VmManager {
    pub fn new() -> Self {
        let firecracker_available = Command::new("firecracker/firecracker")
            .arg("--version")
            .output()
            .is_ok();
        if !firecracker_available {
            panic!("Firecracker executable not found or not runnable.");
        }
        Self {
            instances: Arc::new(Mutex::new(HashMap::new())),
            next_cid: Arc::new(Mutex::new(100)),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            vsock_listener: Arc::new(Mutex::new(None)),
            http_client: Arc::new(Client::new()),
        }
    }

    // --- Public API ---

    pub async fn create_vm(
        &self,
        vm_id: String,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        firecracker::create_vm_internal(self, vm_id).await
    }

    pub async fn destroy_vm(
        &self,
        vm_id: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        firecracker::destroy_vm_internal(self, vm_id).await
    }

    pub fn list_vms(&self) -> Vec<String> {
        firecracker::list_vms_internal(self)
    }

    pub async fn execute_vm_command(
        &self,
        vm_id: &str,
        command: String,
        args: Vec<String>,
        working_dir: Option<String>,
        timeout_seconds: Option<u64>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.execute_command_with_retry(vm_id, command, args, working_dir, timeout_seconds, 3)
            .await
    }

    pub fn start_http_proxy_server(
        &self,
        port: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        http_proxy::start_http_proxy_server_internal(
            self.instances.clone(),
            self.http_client.clone(),
            self.shutdown_flag.clone(),
            port,
        )
    }

    pub async fn spawn_command(
        &self,
        vm_id: &str,
        command: String,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        firecracker::spawn_command_internal(self, vm_id, command, vec![], None, Some(30)).await
    }

    pub async fn list_spawned_processes(
        &self,
        vm_id: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        firecracker::list_spawned_processes_internal(self, vm_id).await
    }

    pub async fn stop_spawned_process(
        &self,
        vm_id: &str,
        process_id: &str,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        firecracker::stop_spawned_process_internal(self, vm_id, process_id).await
    }

    pub fn start_log_listener_server(
        &self,
        port: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        log_listener::start_log_listener_server(
            self.instances.clone(),
            self.shutdown_flag.clone(),
            port,
        )
    }

    pub fn start_vsock_server(
        &self,
        port: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, port)?;
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
                                log::error!("Error handling VM connection: {}", e);
                            }
                        });
                    }
                    Err(e) => log::error!("Error accepting VSOCK connection: {}", e),
                }
            }
        });
        Ok(())
    }

    // --- Shutdown and Cleanup ---

    pub fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        let vm_pids: Vec<(String, Option<u32>)> = {
            let instances_guard = self.instances.lock().unwrap();
            instances_guard
                .iter()
                .map(|(id, instance)| (id.clone(), instance.pid))
                .collect()
        };

        if vm_pids.is_empty() {
            return;
        }

        for (vm_id, pid_opt) in vm_pids {
            if let Some(pid) = pid_opt {
                if Self::terminate_process(pid, "TERM").is_err() {
                    Self::terminate_process(pid, "KILL").ok();
                } else {
                    thread::sleep(Duration::from_millis(500));
                    if Self::is_process_running(pid) {
                        Self::terminate_process(pid, "KILL").ok();
                    }
                }
            }
        }
        self.instances.lock().unwrap().clear();
        *self.vsock_listener.lock().unwrap() = None;
    }

    pub fn emergency_cleanup() {
        if let Ok(output) = Command::new("pgrep").arg("-f").arg("firecracker").output() {
            if output.status.success() {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| line.trim().parse::<u32>().ok())
                    .for_each(|pid| {
                        Self::terminate_process(pid, "KILL").ok();
                    });
            }
        }
    }

    // --- Internal Logic ---

    async fn execute_command_with_retry(
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
            match firecracker::execute_command_in_vm_internal(
                self,
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
                    if (error_msg.contains("sending on a closed channel")
                        || error_msg.contains("VM disconnected"))
                        && retries < max_retries
                    {
                        self.reconnect_vm_channels(vm_id).await.ok();
                        let delay = Duration::from_millis(1000 * 2u64.pow(retries));
                        tokio::time::sleep(delay).await;
                        retries += 1;
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }

    async fn reconnect_vm_channels(
        &self,
        vm_id: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if firecracker::check_vm_health_internal(self, vm_id).await {
            return Ok(());
        }
        self.destroy_vm(vm_id).await.ok();
        self.create_vm(vm_id.to_string()).await.map(|_| ())
    }

    fn handle_vm_connection(
        stream: &mut VsockStream,
        instances: Arc<Mutex<HashMap<String, VmInstance>>>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut buffer = [0; 4096];
        if let Ok(n) = stream.read(&mut buffer) {
            if let Ok(msg_value) = serde_json::from_slice::<Value>(&buffer[..n]) {
                if let Some(msg_type) = msg_value["type"].as_str() {
                    match msg_type {
                        "command_result" => {
                            if let Ok(cmd_result) =
                                serde_json::from_value::<VmCommandResult>(msg_value)
                            {
                                let vm_id = ""; // This part of the logic needs reassessment.
                                if let Some(vm_instance) = instances.lock().unwrap().get(vm_id) {
                                    if let Some(sender) = vm_instance
                                        .result_receiver
                                        .lock()
                                        .unwrap()
                                        .get(&cmd_result.id)
                                    {
                                        sender.send(cmd_result).ok();
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn terminate_process(pid: u32, signal: &str) -> Result<(), std::io::Error> {
        let result = Command::new("kill")
            .arg(format!("-{}", signal))
            .arg(pid.to_string())
            .status();

        match &result {
            Ok(_) => log::debug!("Successfully sent signal '{}' to process {}", signal, pid),
            Err(e) => log::error!(
                "Failed to send signal '{}' to process {}: {:?}",
                signal,
                pid,
                e
            ),
        }

        result.map(|_| ())
    }

    fn is_process_running(pid: u32) -> bool {
        let result = Command::new("kill").arg("-0").arg(pid.to_string()).status();

        match result {
            Ok(status) if status.success() => {
                log::debug!("Process {} is running", pid);
                true
            }
            Ok(_) => {
                log::debug!("Process {} is not running", pid);
                false
            }
            Err(e) => {
                log::error!("Failed to check if process {} is running: {:?}", pid, e);
                false
            }
        }
    }
}

impl Drop for VmManager {
    fn drop(&mut self) {
        if !self.shutdown_flag.load(Ordering::SeqCst) {
            self.shutdown();
        }
    }
}
