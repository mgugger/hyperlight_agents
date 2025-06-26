use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::io::{Read, Write};
use tokio::time::sleep;
use serde::{Deserialize, Serialize};
use vsock::{VsockStream, VsockListener};
use uuid::Uuid;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "vm-agent")]
#[command(about = "Fast VM agent for hyperlight agents")]
struct Args {
    #[arg(short, long, default_value = "vm-agent")]
    vm_id: String,
    
    #[arg(short, long, default_value = "100")]
    cid: u32,
    
    #[arg(short, long, default_value = "2")]
    host_cid: u32,
    
    #[arg(short, long, default_value = "1234")]
    register_port: u32,
    
    #[arg(long, default_value = "1235")]
    command_port: u32,
    
    #[arg(long, default_value = "5")]
    register_interval: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum Message {
    #[serde(rename = "register")]
    Register {
        vm_id: String,
        cid: u32,
    },
    #[serde(rename = "register_ack")]
    RegisterAck {
        vm_id: String,
        status: String,
    },
    #[serde(rename = "execute_command")]
    ExecuteCommand {
        id: String,
        command: String,
        args: Option<Vec<String>>,
        working_dir: Option<String>,
        timeout_seconds: Option<u64>,
    },
    #[serde(rename = "command_result")]
    CommandResult {
        id: String,
        vm_id: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration: u64,
    },
}

struct VmAgent {
    vm_id: String,
    cid: u32,
    host_cid: u32,
    register_port: u32,
    command_port: u32,
    register_interval: Duration,
}

impl VmAgent {
    fn new(args: Args) -> Self {
        Self {
            vm_id: args.vm_id,
            cid: args.cid,
            host_cid: args.host_cid,
            register_port: args.register_port,
            command_port: args.command_port,
            register_interval: Duration::from_secs(args.register_interval),
        }
    }

    async fn run(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        println!("Starting VM Agent: {} (CID: {})", self.vm_id, self.cid);
        
        // Start registration task
        let registration_task = self.start_registration_loop();
        
        // Start command listener
        let command_task = self.start_command_listener();
        
        // Run both tasks concurrently
        tokio::select! {
            result = registration_task => {
                eprintln!("Registration task ended: {:?}", result);
            }
            result = command_task => {
                eprintln!("Command listener ended: {:?}", result);
            }
        }
        
        Ok(())
    }

    async fn start_registration_loop(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        loop {
            if let Err(e) = self.register_with_host().await {
                eprintln!("Failed to register with host: {}", e);
            }
            
            sleep(self.register_interval).await;
        }
    }

    async fn register_with_host(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let message = Message::Register {
            vm_id: self.vm_id.clone(),
            cid: self.cid,
        };
        
        let json_msg = serde_json::to_string(&message)?;
        
        match VsockStream::connect_with_cid_port(self.host_cid, self.register_port) {
            Ok(mut stream) => {
                stream.write_all(json_msg.as_bytes())?;
                
                // Try to read response (with timeout)
                let mut buffer = [0; 1024];
                match stream.read(&mut buffer) {
                    Ok(n) if n > 0 => {
                        let response = String::from_utf8_lossy(&buffer[..n]);
                        println!("Registration response: {}", response);
                    }
                    _ => {
                        // No response or empty response - that's okay
                    }
                }
                
                Ok(())
            }
            Err(e) => {
                Err(format!("Failed to connect to host for registration: {}", e).into())
            }
        }
    }

    async fn start_command_listener(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = VsockListener::bind_with_cid_port(self.cid, self.command_port)?;
        println!("Command listener started on CID {} port {}", self.cid, self.command_port);
        
        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    let vm_id = self.vm_id.clone();
                    let host_cid = self.host_cid;
                    let register_port = self.register_port;
                    
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_command_connection(&mut stream, vm_id, host_cid, register_port).await {
                            eprintln!("Error handling command connection: {}", e);
                        }
                    });
                }
                Err(e) => {
                    eprintln!("Error accepting command connection: {}", e);
                }
            }
        }
        
        Ok(())
    }

    async fn handle_command_connection(
        stream: &mut VsockStream,
        vm_id: String,
        host_cid: u32,
        register_port: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut buffer = [0; 4096];
        
        match stream.read(&mut buffer) {
            Ok(n) if n > 0 => {
                let message_str = String::from_utf8_lossy(&buffer[..n]);
                
                if let Ok(message) = serde_json::from_str::<Message>(&message_str) {
                    match message {
                        Message::ExecuteCommand { id, command, args, working_dir, timeout_seconds } => {
                            let result = Self::execute_command(
                                &id,
                                &command,
                                args.unwrap_or_default(),
                                working_dir,
                                timeout_seconds,
                            ).await;
                            
                            let result_msg = Message::CommandResult {
                                id,
                                vm_id,
                                exit_code: result.exit_code,
                                stdout: result.stdout,
                                stderr: result.stderr,
                                duration: result.duration,
                            };
                            
                            // Send result back to host
                            if let Err(e) = Self::send_result_to_host(result_msg, host_cid, register_port).await {
                                eprintln!("Failed to send result to host: {}", e);
                            }
                        }
                        _ => {
                            println!("Received unexpected message type: {:?}", message);
                        }
                    }
                }
            }
            _ => {
                // No data or error reading
            }
        }
        
        Ok(())
    }

    async fn execute_command(
        command_id: &str,
        command: &str,
        args: Vec<String>,
        working_dir: Option<String>,
        timeout_seconds: Option<u64>,
    ) -> CommandResult {
        println!("Executing command {}: {} {:?}", command_id, command, args);
        
        let start_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }
        
        cmd.stdout(Stdio::piped())
           .stderr(Stdio::piped());
        
        let output = if let Some(timeout) = timeout_seconds {
            tokio::time::timeout(
                Duration::from_secs(timeout),
                tokio::task::spawn_blocking(move || cmd.output())
            ).await
        } else {
            Ok(tokio::task::spawn_blocking(move || cmd.output()).await?)
        };
        
        let end_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        
        let duration = end_time - start_time;
        
        match output {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);
                
                println!("Command {} completed with exit code {} in {}s", command_id, exit_code, duration);
                
                CommandResult {
                    exit_code,
                    stdout,
                    stderr,
                    duration,
                }
            }
            Ok(Err(e)) => {
                eprintln!("Command {} failed to execute: {}", command_id, e);
                CommandResult {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("Failed to execute command: {}", e),
                    duration,
                }
            }
            Err(_) => {
                eprintln!("Command {} timed out", command_id);
                CommandResult {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: "Command timed out".to_string(),
                    duration,
                }
            }
        }
    }

    async fn send_result_to_host(
        result: Message,
        host_cid: u32,
        register_port: u32,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let json_msg = serde_json::to_string(&result)?;
        
        match VsockStream::connect_with_cid_port(host_cid, register_port) {
            Ok(mut stream) => {
                stream.write_all(json_msg.as_bytes())?;
                Ok(())
            }
            Err(e) => {
                Err(format!("Failed to connect to host for result: {}", e).into())
            }
        }
    }
}

struct CommandResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
    duration: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    let agent = VmAgent::new(args);
    
    agent.run().await
}
