use serde::{Deserialize, Serialize};
use std::process::{Command, Stdio};

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommandResponse {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn execute_command(command: &str) -> CommandResponse {
    log::debug!("Executing command: {}", command);

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

            log::debug!("Command completed with exit code {}", exit_code);

            CommandResponse {
                exit_code,
                stdout,
                stderr,
            }
        }
        Err(e) => {
            log::error!("Failed to execute command: {}", e);
            CommandResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Failed to execute command: {}", e),
            }
        }
    }
}
