use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Stdio};

lazy_static! {
    static ref PROCESS_TABLE: std::sync::Mutex<std::collections::HashMap<u64, (String, std::process::Child)>> =
        std::sync::Mutex::new(std::collections::HashMap::new());
}

use hyperlight_agents_common::{VmCommand, VmCommandMode};

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnedProcessInfo {
    pub id: u64,
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StopProcessResponse {
    pub id: u64,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListSpawnedProcessesRequest;

#[derive(Debug, Serialize, Deserialize)]
pub struct StopSpawnedProcessRequest {
    pub id: u64,
}

use std::thread;
use std::time::{Duration, Instant};

pub fn execute_command(command: &str, timeout_secs: u64) -> CommandResponse {
    log::debug!("Executing command: {}", command);

    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            log::error!("Failed to execute command: {}", e);
            return CommandResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Failed to execute command: {}", e),
            };
        }
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Process exited
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                let exit_code = status.code().unwrap_or(-1);
                log::debug!("Command completed with exit code {}", exit_code);
                return CommandResponse {
                    exit_code,
                    stdout,
                    stderr,
                };
            }
            Ok(None) => {
                // Still running
                if start.elapsed() > Duration::from_secs(timeout_secs) {
                    // Timeout reached, kill the process
                    let _ = child.kill();
                    let _ = child.wait();

                    let mut stdout = String::new();
                    let mut stderr = String::new();
                    if let Some(mut out) = child.stdout.take() {
                        let _ = out.read_to_string(&mut stdout);
                    }
                    if let Some(mut err) = child.stderr.take() {
                        let _ = err.read_to_string(&mut stderr);
                    }

                    log::error!("Command timed out after {} seconds", timeout_secs);
                    log::error!("Partial stdout: {}", stdout);
                    log::error!("Partial stderr: {}", stderr);

                    return CommandResponse {
                        exit_code: -2,
                        stdout,
                        stderr: format!(
                            "Command timed out after {} seconds\n{}",
                            timeout_secs, stderr
                        ),
                    };
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                log::error!("Error waiting for child: {}", e);
                return CommandResponse {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("Error waiting for child: {}", e),
                };
            }
        }
    }
}

/// Spawns a command in the background using VmCommand and returns its ID.
pub fn spawn_command_struct(cmd: &VmCommand) -> Option<SpawnedProcessInfo> {
    log::debug!("Spawning command struct: {:?}", cmd);

    // Build the full command string for shell execution
    let full_command = if cmd.args.is_empty() {
        cmd.command.clone()
    } else {
        let mut s = cmd.command.clone();
        for arg in &cmd.args {
            s.push(' ');
            s.push_str(arg);
        }
        s
    };

    let mut command = Command::new("sh");
    command.arg("-c").arg(full_command);

    if let Some(ref dir) = cmd.working_dir {
        command.current_dir(dir);
    }
    command.stdout(Stdio::null()).stderr(Stdio::null());

    // Optionally handle timeout_seconds (not implemented here)
    match command.spawn() {
        Ok(child) => {
            let id = next_process_id();
            let mut table = PROCESS_TABLE.lock().unwrap();
            table.insert(id, (cmd.command.clone(), child));
            Some(SpawnedProcessInfo {
                id,
                command: cmd.command.clone(),
            })
        }
        Err(e) => {
            log::error!("Failed to spawn command: {}", e);
            None
        }
    }
}

fn next_process_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Spawns a command in the background and returns its ID.
pub fn spawn_command(command: &str) -> Option<SpawnedProcessInfo> {
    log::debug!("Spawning command: {}", command);
    match Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => {
            let id = next_process_id();
            let mut table = PROCESS_TABLE.lock().unwrap();
            table.insert(id, (command.to_string(), child));
            Some(SpawnedProcessInfo {
                id,
                command: command.to_string(),
            })
        }
        Err(e) => {
            log::error!("Failed to spawn command: {}", e);
            None
        }
    }
}

/// Lists all currently spawned processes.
pub fn list_spawned_processes() -> Vec<SpawnedProcessInfo> {
    log::debug!("List spawned processes");
    let table = PROCESS_TABLE.lock().unwrap();
    table
        .iter()
        .map(|(id, (cmd, _))| SpawnedProcessInfo {
            id: *id,
            command: cmd.clone(),
        })
        .collect()
}

/// Stops a spawned process by ID and returns its output.
pub fn stop_spawned_process(id: u64) -> Option<StopProcessResponse> {
    log::debug!("Stopping spawned process {}", id);
    let mut table = PROCESS_TABLE.lock().unwrap();
    if let Some((command, mut child)) = table.remove(&id) {
        match child.kill() {
            Ok(_) => match child.wait_with_output() {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let exit_code = output.status.code().unwrap_or(-1);
                    Some(StopProcessResponse {
                        id,
                        exit_code,
                        stdout,
                        stderr,
                    })
                }
                Err(e) => {
                    log::error!("Failed to collect output for process {}: {}", id, e);
                    Some(StopProcessResponse {
                        id,
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: format!("Failed to collect output: {}", e),
                    })
                }
            },
            Err(e) => {
                log::error!("Failed to kill process {}: {}", id, e);
                Some(StopProcessResponse {
                    id,
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("Failed to kill process: {}", e),
                })
            }
        }
    } else {
        None
    }
}
