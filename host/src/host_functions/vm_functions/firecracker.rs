use super::{VmInstance, VmManager};
use chrono::Utc;
use hyperlight_agents_common::{VmCommand, VmCommandMode, VmCommandResult};
use memfd::{Memfd, MemfdOptions};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

pub(crate) async fn create_vm_internal(
    manager: &VmManager,
    vm_id: String,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let cid = {
        let mut next_cid = manager.next_cid.lock().unwrap();
        let current_cid = *next_cid;
        *next_cid += 1;
        current_cid
    };

    let temp_dir = TempDir::new()?;
    let (command_sender, command_receiver) = mpsc::channel::<VmCommand>();

    let (vm_process, rootfs_path) = start_firecracker_vm(temp_dir.path(), &vm_id, cid)?;

    let vm_instance = VmInstance {
        vm_id: vm_id.clone(),
        cid,
        pid: vm_process,
        temp_dir,
        command_sender,
        result_receiver: Arc::new(Mutex::new(HashMap::new())),
        memfd_rootfs: None,
        rootfs_symlink: rootfs_path,
    };

    {
        let mut instances = manager.instances.lock().unwrap();
        instances.insert(vm_id.clone(), vm_instance);
    }

    start_command_processor(
        manager.instances.clone(),
        manager.shutdown_flag.clone(),
        vm_id.clone(),
        command_receiver,
    );

    Ok(format!("VM {} created with CID {}", vm_id, cid))
}

pub(crate) fn start_firecracker_vm(
    vm_dir: &Path,
    vm_id: &str,
    cid: u32,
) -> Result<(Option<u32>, Option<PathBuf>), Box<dyn std::error::Error + Send + Sync>> {
    let vm_images_dir = Path::new("firecracker");
    let kernel_path = vm_images_dir.join("vmlinux");
    let source_rootfs_path = vm_images_dir.join("rootfs.squashfs");
    let config_path = vm_dir.join("firecracker-config.json");

    if !kernel_path.exists() {
        return Err(format!("Kernel image not found at: {}", kernel_path.display()).into());
    }
    if !source_rootfs_path.exists() {
        return Err(format!(
            "Rootfs image not found at: {}",
            source_rootfs_path.display()
        )
        .into());
    }

    let config = serde_json::json!({
        "boot-source": {
            "kernel_image_path": kernel_path.to_str().unwrap(),
            "boot_args": "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/init root=/dev/vda rootfstype=squashfs ro"
        },
        "drives": [{
            "drive_id": "rootfs",
            "path_on_host": source_rootfs_path.to_str().unwrap(),
            "is_root_device": true,
            "is_read_only": true
        }],
        "machine-config": {
            "vcpu_count": 1,
            "mem_size_mib": 512,
            "smt": false
        },
        "vsock": {
            "guest_cid": cid,
            "uds_path": format!("{}/vsock.sock", vm_dir.display())
        }
    });

    std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;

    let devnull = File::create("/dev/null")?;
    let mut cmd = Command::new("firecracker/firecracker");
    cmd.arg("--api-sock")
        .arg(format!("{}/firecracker.sock", vm_dir.display()))
        .arg("--config-file")
        .arg(&config_path)
        .stdout(devnull.try_clone()?)
        .stderr(devnull.try_clone()?)
        .stdin(devnull);

    match cmd.spawn() {
        Ok(child) => {
            thread::sleep(Duration::from_secs(2));
            Ok((Some(child.id()), Some(source_rootfs_path)))
        }
        Err(e) => {
            log::error!("Failed to start Firecracker VM: {}", e);
            Err(e.into())
        }
    }
}

// Removed: create_memfd_rootfs, not needed for squashfs readonly rootfs.

fn start_command_processor(
    instances: Arc<Mutex<HashMap<String, VmInstance>>>,
    shutting_down: Arc<AtomicBool>,
    vm_id: String,
    receiver: mpsc::Receiver<VmCommand>,
) {
    thread::spawn(move || {
        for command in receiver {
            if shutting_down.load(Ordering::SeqCst) {
                break;
            }

            let (vsock_socket_path, result_sender) = {
                let instances_guard = instances.lock().unwrap();
                if let Some(vm_instance) = instances_guard.get(&vm_id) {
                    let socket_path =
                        format!("{}/vsock.sock", vm_instance.temp_dir.path().display());
                    let sender = vm_instance
                        .result_receiver
                        .lock()
                        .unwrap()
                        .get(&command.id)
                        .cloned();
                    (socket_path, sender)
                } else {
                    continue;
                }
            };

            let mut vm_result = VmCommandResult {
                id: command.id.clone(),
                exit_code: -1,
                stdout: String::new(),
                stderr: String::new(),
                error: None,
            };

            if !Path::new(&vsock_socket_path).exists() {
                vm_result.error = Some(format!("VSOCK socket not found: {}", vsock_socket_path));
                if let Some(sender) = result_sender {
                    sender.send(vm_result).ok();
                }
                continue;
            }

            match std::os::unix::net::UnixStream::connect(&vsock_socket_path) {
                Ok(mut stream) => {
                    stream.set_nonblocking(false).ok();
                    let handshake = "CONNECT 1234\n";
                    if stream.write_all(handshake.as_bytes()).is_err() {
                        vm_result.error = Some("Handshake send failed".to_string());
                    } else {
                        let mut h_buf = [0; 256];
                        if stream.read(&mut h_buf).is_ok() {
                            let vsock_request =
                                crate::host_functions::vm_functions::VsockRequest::Command(command);
                            let command_str = serde_json::to_string(&vsock_request).unwrap();

                            if stream.write_all(command_str.as_bytes()).is_ok()
                                && stream.flush().is_ok()
                            {
                                let mut response_buffer = Vec::new();
                                if stream.read_to_end(&mut response_buffer).is_ok() {
                                    if let Ok(response_str) = String::from_utf8(response_buffer) {
                                        if let Ok(json) =
                                            serde_json::from_str::<Value>(&response_str)
                                        {
                                            vm_result.exit_code =
                                                json["exit_code"].as_i64().unwrap_or(-1) as i32;
                                            vm_result.stdout =
                                                json["stdout"].as_str().unwrap_or("").to_string();
                                            vm_result.stderr =
                                                json["stderr"].as_str().unwrap_or("").to_string();
                                        } else {
                                            vm_result.error =
                                                Some("Failed to parse JSON response".to_string());
                                        }
                                    } else {
                                        vm_result.error =
                                            Some("Invalid UTF-8 in response".to_string());
                                    }
                                } else {
                                    vm_result.error = Some("Failed to read response".to_string());
                                }
                            } else {
                                vm_result.error = Some("Failed to send command".to_string());
                            }
                        } else {
                            vm_result.error = Some("Handshake read failed".to_string());
                        }
                    }
                }
                Err(e) => {
                    vm_result.error = Some(format!("Connection failed: {}", e));
                }
            }

            if let Some(sender) = result_sender {
                sender.send(vm_result).ok();
            }
        }
    });
}

pub(crate) async fn execute_command_in_vm_internal(
    manager: &VmManager,
    vm_id: &str,
    command: String,
    args: Vec<String>,
    working_dir: Option<String>,
    timeout_seconds: Option<u64>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let cmd_id = format!("cmd_{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));

    let (command_sender, result_receiver) = {
        let instances = manager.instances.lock().unwrap();
        if let Some(vm_instance) = instances.get(vm_id) {
            let (tx, rx) = mpsc::channel();
            vm_instance
                .result_receiver
                .lock()
                .unwrap()
                .insert(cmd_id.clone(), tx);
            (vm_instance.command_sender.clone(), rx)
        } else {
            return Err(format!("VM {} not found", vm_id).into());
        }
    };

    let vm_command = VmCommand {
        id: cmd_id.clone(),
        command,
        args,
        working_dir,
        timeout_seconds,
        mode: VmCommandMode::Foreground,
    };

    command_sender
        .send(vm_command)
        .map_err(|e| format!("Failed to send command to VM: {}", e))?;

    let timeout_duration = Duration::from_secs(timeout_seconds.unwrap_or(30));
    let start_time = Instant::now();

    loop {
        match result_receiver.try_recv() {
            Ok(result) => {
                manager
                    .instances
                    .lock()
                    .unwrap()
                    .get(vm_id)
                    .map(|vm| vm.result_receiver.lock().unwrap().remove(&cmd_id));
                if result.exit_code == 0 {
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
                    manager
                        .instances
                        .lock()
                        .unwrap()
                        .get(vm_id)
                        .map(|vm| vm.result_receiver.lock().unwrap().remove(&cmd_id));
                    return Err("Command execution timed out".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("VM disconnected while waiting for command result".into());
            }
        }
    }
}

/// Spawns a command in the VM agent and returns the command ID (or PID if agent returns it)
pub(crate) async fn spawn_command_internal(
    manager: &VmManager,
    vm_id: &str,
    command: String,
    args: Vec<String>,
    working_dir: Option<String>,
    timeout_seconds: Option<u64>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let cmd_id = format!("cmd_{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
    let (command_sender, result_receiver) = {
        let instances = manager.instances.lock().unwrap();
        if let Some(vm_instance) = instances.get(vm_id) {
            let (tx, rx) = mpsc::channel();
            vm_instance
                .result_receiver
                .lock()
                .unwrap()
                .insert(cmd_id.clone(), tx);
            (vm_instance.command_sender.clone(), rx)
        } else {
            return Err(format!("VM {} not found", vm_id).into());
        }
    };

    let vm_command = VmCommand {
        id: cmd_id.clone(),
        command,
        args,
        working_dir,
        timeout_seconds,
        mode: VmCommandMode::Spawn,
    };

    command_sender
        .send(vm_command)
        .map_err(|e| format!("Failed to send spawn command to VM: {}", e))?;

    // For spawn, we just return the command id immediately
    Ok(cmd_id)
}

/// Lists spawned processes in the VM agent
pub(crate) async fn list_spawned_processes_internal(
    manager: &VmManager,
    vm_id: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let (command_sender, result_receiver) = {
        let instances = manager.instances.lock().unwrap();
        if let Some(vm_instance) = instances.get(vm_id) {
            let (tx, rx) = mpsc::channel();
            // Use a special id for listing
            vm_instance
                .result_receiver
                .lock()
                .unwrap()
                .insert("list_spawned_processes".to_string(), tx);
            (vm_instance.command_sender.clone(), rx)
        } else {
            return Err(format!("VM {} not found", vm_id).into());
        }
    };

    let vm_command = VmCommand {
        id: "list_spawned_processes".to_string(),
        command: "list_spawned_processes".to_string(),
        args: vec![],
        working_dir: None,
        timeout_seconds: Some(30),
        mode: VmCommandMode::Foreground,
    };

    command_sender
        .send(vm_command)
        .map_err(|e| format!("Failed to send list_spawned_processes to VM: {}", e))?;

    let timeout_duration = Duration::from_secs(30);
    let start_time = Instant::now();

    loop {
        match result_receiver.try_recv() {
            Ok(result) => {
                // Expect stdout to be a JSON array of process IDs
                log::debug!(
                    "Result from list spawned processes initiated for {:?}",
                    result
                );
                let trimmed = result.stdout.trim();
                if trimmed.is_empty() {
                    return Ok(Vec::new());
                }
                match serde_json::from_str::<Vec<String>>(&trimmed) {
                    Ok(list) => return Ok(list),
                    Err(e) => {
                        return Err(
                            format!("Failed to parse process list from VM agent: {:?}", e).into(),
                        )
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                if start_time.elapsed() > timeout_duration {
                    return Err("List spawned processes timed out".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("VM disconnected while waiting for process list".into());
            }
        }
    }
}

/// Stops a spawned process in the VM agent
pub(crate) async fn stop_spawned_process_internal(
    manager: &VmManager,
    vm_id: &str,
    process_id: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let cmd_id = format!("stop_{}", process_id);

    let (command_sender, result_receiver) = {
        let instances = manager.instances.lock().unwrap();
        if let Some(vm_instance) = instances.get(vm_id) {
            let (tx, rx) = mpsc::channel();
            vm_instance
                .result_receiver
                .lock()
                .unwrap()
                .insert(cmd_id.clone(), tx);
            (vm_instance.command_sender.clone(), rx)
        } else {
            return Err(format!("VM {} not found", vm_id).into());
        }
    };

    let vm_command = VmCommand {
        id: cmd_id.clone(),
        command: "stop_spawned_process".to_string(),
        args: vec![process_id.to_string()],
        working_dir: None,
        timeout_seconds: Some(30),
        mode: VmCommandMode::Foreground,
    };

    command_sender
        .send(vm_command)
        .map_err(|e| format!("Failed to send stop_spawned_process to VM: {}", e))?;

    let timeout_duration = Duration::from_secs(30);
    let start_time = Instant::now();

    loop {
        match result_receiver.try_recv() {
            Ok(result) => {
                if result.exit_code == 0 {
                    return Ok(result.stdout);
                } else {
                    return Err(format!(
                        "Stop process failed with exit code {}: {}",
                        result.exit_code, result.stderr
                    )
                    .into());
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                if start_time.elapsed() > timeout_duration {
                    return Err("Stop spawned process timed out".into());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err("VM disconnected while waiting for stop process result".into());
            }
        }
    }
}

pub(crate) async fn destroy_vm_internal(
    manager: &VmManager,
    vm_id: &str,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut instances = manager.instances.lock().unwrap();
    if let Some(vm_instance) = instances.remove(vm_id) {
        if let Some(pid) = vm_instance.pid {
            terminate_process(pid, "KILL").ok();
        }
        if let Some(symlink_path) = &vm_instance.rootfs_symlink {
            std::fs::remove_file(symlink_path).ok();
        }
        Ok(format!("VM {} destroyed", vm_id))
    } else {
        Err(format!("VM {} not found", vm_id).into())
    }
}

pub(crate) fn list_vms_internal(manager: &VmManager) -> Vec<String> {
    manager.instances.lock().unwrap().keys().cloned().collect()
}

pub(crate) async fn check_vm_health_internal(manager: &VmManager, vm_id: &str) -> bool {
    if let Some(vm_instance) = manager.instances.lock().unwrap().get(vm_id) {
        let health_cmd = VmCommand {
            id: "health-check".to_string(),
            command: "echo".to_string(),
            args: vec!["healthy".to_string()],
            working_dir: None,
            timeout_seconds: Some(30),
            mode: VmCommandMode::Foreground,
        };
        return vm_instance.command_sender.send(health_cmd).is_ok();
    }
    false
}

pub(crate) fn terminate_process(pid: u32, signal: &str) -> Result<(), std::io::Error> {
    log::debug!(
        "Attempting to send signal '{}' to process with PID {}",
        signal,
        pid
    );
    match Command::new("kill")
        .arg(format!("-{}", signal))
        .arg(pid.to_string())
        .output()
    {
        Ok(_) => {
            log::debug!(
                "Successfully sent signal '{}' to process with PID {}",
                signal,
                pid
            );
            Ok(())
        }
        Err(e) => {
            log::error!(
                "Failed to send signal '{}' to process with PID {}: {:?}",
                signal,
                pid,
                e
            );
            Err(e)
        }
    }
}
