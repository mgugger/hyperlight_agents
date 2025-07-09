use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Starts the log listener server lazily, waiting for a VM to exist to determine the socket path.
/// This matches the pattern used by the HTTP proxy server.
pub(crate) fn start_log_listener_server(
    instances: Arc<Mutex<HashMap<String, super::VmInstance>>>,
    shutdown_flag: Arc<AtomicBool>,
    port: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    thread::spawn(move || {
        loop {
            if shutdown_flag.load(Ordering::Relaxed) {
                break;
            }

            let (socket_path, vm_id_opt) = {
                let instances_guard = instances.lock().unwrap();
                if let Some((vm_id, vm_instance)) = instances_guard.iter().next() {
                    let base_path = vm_instance.temp_dir.path().join("vsock.sock");
                    (
                        Some(format!("{}_{}", base_path.display(), port)),
                        Some(vm_id.clone()),
                    )
                } else {
                    (None, None)
                }
            };

            if let (Some(socket_path), Some(vm_id)) = (socket_path, vm_id_opt) {
                if let Err(e) =
                    run_log_listener_unix_server(&socket_path, &vm_id, shutdown_flag.clone())
                {
                    eprintln!("Log listener Unix server failed: {}", e);
                }
                // Once we've started (or failed), break the loop.
                break;
            } else {
                // No VMs yet, wait a bit before checking again.
                thread::sleep(Duration::from_millis(200));
            }
        }
    });

    Ok(())
}

/// Runs the Unix socket server for the log listener.
fn run_log_listener_unix_server(
    socket_path: &str,
    vm_id: &str,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Clean up any old socket file.
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    println!("Log Listener listening on Unix socket: {}", socket_path);

    // Set a timeout so the accept loop doesn't block forever, allowing shutdown check.
    listener.set_nonblocking(true)?;

    for stream in listener.incoming() {
        if shutdown_flag.load(Ordering::Relaxed) {
            break;
        }

        match stream {
            Ok(mut stream) => {
                let vm_id = vm_id.to_string();
                thread::spawn(move || {
                    if let Err(e) = handle_log_listener_unix_connection(&mut stream, &vm_id) {
                        eprintln!("Error handling log listener connection: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No incoming connection, sleep and check for shutdown again.
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => {
                eprintln!("Error accepting log listener connection: {}", e);
                // Potentially break here if the listener is in an unrecoverable state.
            }
        }
    }

    Ok(())
}

/// Handles an individual connection to the log listener.
fn handle_log_listener_unix_connection(
    stream: &mut UnixStream,
    vm_id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];
    let mut incomplete = String::new();

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break, // Connection closed cleanly.
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if let Ok(log_message) = String::from_utf8(buffer.clone()) {
                    incomplete.push_str(&log_message);

                    let mut last_index = 0;
                    for (idx, c) in incomplete.char_indices() {
                        if c == '\n' || c == '\r' {
                            let line = &incomplete[last_index..idx];
                            if !line.trim().is_empty() {
                                println!("[{}] {}", vm_id, line);
                            }
                            last_index = idx + 1;
                        }
                    }
                    // Save any incomplete line for the next read
                    incomplete = incomplete[last_index..].to_string();
                    buffer.clear();
                }
            }
            Err(e) => {
                eprintln!("Error reading from log listener unix stream: {}", e);
                break;
            }
        }
    }

    // Print any remaining incomplete line
    if !incomplete.trim().is_empty() {
        println!("[{}] {}", vm_id, incomplete);
    }

    Ok(())
}
