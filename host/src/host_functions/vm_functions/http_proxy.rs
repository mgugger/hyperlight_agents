use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{VmInstance, VsockRequest, VsockResponse};

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpProxyRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HttpProxyResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub error: Option<String>,
}

pub(crate) fn start_http_proxy_server_internal(
    instances: Arc<Mutex<HashMap<String, VmInstance>>>,
    http_client: Arc<Client>,
    shutdown_flag: Arc<AtomicBool>,
    port: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    thread::spawn(move || {
        // Wait for a VM to be created to determine the socket path.
        // This is a bit of a hack; a more robust solution might involve a shared config.
        loop {
            if shutdown_flag.load(Ordering::Relaxed) {
                break;
            }

            let socket_path = {
                let instances_guard = instances.lock().unwrap();
                if let Some((_, vm_instance)) = instances_guard.iter().next() {
                    // All VMs share the same proxy, so we just need one to find the base path.
                    let base_path = vm_instance.temp_dir.path().join("vsock.sock");
                    Some(format!("{}_{}", base_path.display(), port))
                } else {
                    None
                }
            };

            if let Some(socket_path) = socket_path {
                if let Err(e) = run_http_proxy_unix_server(
                    &socket_path,
                    http_client.clone(),
                    shutdown_flag.clone(),
                ) {
                    eprintln!("HTTP proxy Unix server failed: {}", e);
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

fn run_http_proxy_unix_server(
    socket_path: &str,
    http_client: Arc<Client>,
    shutdown_flag: Arc<AtomicBool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Clean up any old socket file.
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    println!("HTTP Proxy listening on Unix socket: {}", socket_path);

    // Set a timeout so the accept loop doesn't block forever, allowing shutdown check.
    listener.set_nonblocking(true)?;

    for stream in listener.incoming() {
        if shutdown_flag.load(Ordering::Relaxed) {
            break;
        }

        match stream {
            Ok(mut stream) => {
                let client = http_client.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_http_proxy_unix_connection(&mut stream, client) {
                        eprintln!("Error handling HTTP proxy connection: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No incoming connection, sleep and check for shutdown again.
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => {
                eprintln!("Error accepting HTTP proxy connection: {}", e);
                // Potentially break here if the listener is in an unrecoverable state.
            }
        }
    }

    Ok(())
}

fn handle_http_proxy_unix_connection(
    stream: &mut UnixStream,
    http_client: Arc<Client>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // It's possible for the client to send the request in multiple chunks.
    // We need to buffer until we have a complete JSON object.
    let mut buffer = Vec::new();
    let mut chunk = [0; 4096];

    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break, // Connection closed cleanly.
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                // Try to deserialize. If it works, we have a full message.
                if let Ok(vsock_request) = serde_json::from_slice::<VsockRequest>(&buffer) {
                    if let VsockRequest::HttpProxy(proxy_request) = vsock_request {
                        let response = execute_http_request(proxy_request, &http_client);
                        let vsock_response = VsockResponse::HttpProxy(response);
                        let response_json = serde_json::to_string(&vsock_response)?;

                        stream.write_all(response_json.as_bytes())?;
                        stream.flush()?;
                    }
                    // Message handled, we can clear the buffer or break.
                    // Since the agent likely closes the connection after one request/response,
                    // we'll break here.
                    break;
                }
                // If deserialization fails, we assume the message is incomplete and loop to read more.
            }
            Err(e) => {
                eprintln!("Error reading from HTTP proxy unix stream: {}", e);
                break;
            }
        }
    }

    Ok(())
}

fn execute_http_request(
    proxy_request: HttpProxyRequest,
    http_client: &Client,
) -> HttpProxyResponse {
    // This function needs an async runtime to execute the request.
    // We can spawn a new one for each request. This is not the most efficient
    // way, but it's simple and avoids threading `await`s all the way up.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let method = match proxy_request.method.to_uppercase().as_str() {
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            "HEAD" => reqwest::Method::HEAD,
            "OPTIONS" => reqwest::Method::OPTIONS,
            "PATCH" => reqwest::Method::PATCH,
            _ => reqwest::Method::GET, // Default to GET
        };

        let mut request_builder = http_client.request(method, &proxy_request.url);

        for (name, value) in &proxy_request.headers {
            request_builder = request_builder.header(name, value);
        }

        if let Some(body_data) = proxy_request.body {
            request_builder = request_builder.body(body_data);
        }

        match request_builder.send().await {
            Ok(response) => {
                let status_code = response.status().as_u16();
                let mut headers = HashMap::new();
                for (name, value) in response.headers().iter() {
                    if let Ok(value_str) = value.to_str() {
                        headers.insert(name.to_string(), value_str.to_string());
                    }
                }
                match response.bytes().await {
                    Ok(body_bytes) => HttpProxyResponse {
                        status_code,
                        headers,
                        body: body_bytes.to_vec(),
                        error: None,
                    },
                    Err(e) => HttpProxyResponse {
                        status_code: 500,
                        headers: HashMap::new(),
                        body: Vec::new(),
                        error: Some(format!("Failed to read response body: {}", e)),
                    },
                }
            }
            Err(e) => HttpProxyResponse {
                status_code: 500, // Or a more specific error code if possible
                headers: HashMap::new(),
                body: Vec::new(),
                error: Some(format!("HTTP request failed: {}", e)),
            },
        }
    })
}
