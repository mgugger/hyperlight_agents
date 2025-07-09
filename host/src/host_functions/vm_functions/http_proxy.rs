use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::net::TcpStream;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
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
        println!("Host proxy thread started for handling HTTP proxy requests.");
        // Wait for a VM to be created to determine the socket path.
        loop {
            if shutdown_flag.load(Ordering::Relaxed) {
                println!("Shutdown flag detected, stopping server initialization.");
                break;
            }

            let socket_path = {
                let instances_guard = instances.lock().unwrap();
                if let Some((_, vm_instance)) = instances_guard.iter().next() {
                    let base_path = vm_instance.temp_dir.path().join("vsock.sock");
                    Some(format!("{}_{}", base_path.display(), port))
                } else {
                    None
                }
            };

            if let Some(socket_path) = socket_path {
                println!("Computed socket path: {}", socket_path);
                println!(
                    "Attempting to start HTTP proxy Unix server at socket path: {}",
                    socket_path
                );
                if let Err(e) = run_http_proxy_unix_server(
                    &socket_path,
                    http_client.clone(),
                    shutdown_flag.clone(),
                ) {
                    println!("Failed to start HTTP proxy Unix server: {}", e);
                    eprintln!("HTTP proxy Unix server failed: {}", e);
                }
                break;
            } else {
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
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    println!("HTTP Proxy listening on Unix socket: {}", socket_path);
    println!("Server is now ready to accept connections.");

    listener.set_nonblocking(true)?;

    for stream in listener.incoming() {
        if shutdown_flag.load(Ordering::Relaxed) {
            break;
        }

        match stream {
            Ok(mut stream) => {
                let client = http_client.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_http_proxy_or_connect(&mut stream, client) {
                        eprintln!("Error handling HTTP proxy connection: {}", e);
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => {
                eprintln!("Error accepting HTTP proxy connection: {}", e);
            }
        }
    }

    Ok(())
}

fn handle_http_proxy_or_connect(
    stream: &mut UnixStream,
    http_client: Arc<Client>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Peek at the first few bytes to determine if this is a CONNECT or JSON request
    let mut peek_buf = [0u8; 8];
    let n = stream.read(&mut peek_buf)?;
    if n == 0 {
        return Ok(());
    }

    // If it starts with "CONNECT ", handle as a CONNECT tunnel
    if peek_buf[..n].starts_with(b"CONNECT ") {
        // Read the rest of the CONNECT line
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut connect_line = String::from_utf8_lossy(&peek_buf[..n]).to_string();
        reader.read_line(&mut connect_line)?;
        // Example: "CONNECT www.google.com:443 HTTP/1.1\r\n"
        let parts: Vec<&str> = connect_line.trim().split_whitespace().collect();
        if parts.len() < 2 {
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
            return Ok(());
        }
        let target = parts[1];
        println!("CONNECT method received. Target: {}", target);

        // Connect to the target server
        match TcpStream::connect(target) {
            Ok(mut target_stream) => {
                // Send 200 Connection Established
                println!("Connected to target {}", target);
                let _ = stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
                // Relay data in both directions
                relay_bidirectional(stream, &mut target_stream)?;
            }
            Err(e) => {
                eprintln!("Failed to connect to target {}: {}", target, e);
                let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n");
            }
        }
        return Ok(());
    }

    // Otherwise, treat as JSON (legacy)
    let mut buffer = Vec::from(&peek_buf[..n]);
    let mut chunk = [0; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if let Ok(vsock_request) = serde_json::from_slice::<VsockRequest>(&buffer) {
                    if let VsockRequest::HttpProxy(proxy_request) = vsock_request {
                        let response = execute_http_request(proxy_request, &http_client);
                        let vsock_response = VsockResponse::HttpProxy(response);
                        let response_json = serde_json::to_string(&vsock_response)?;
                        stream.write_all(response_json.as_bytes())?;
                        stream.flush()?;
                    }
                    break;
                }
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
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        println!(
            "Executing HTTP request: {} {}",
            proxy_request.method, proxy_request.url
        );
        let method = match proxy_request.method.to_uppercase().as_str() {
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            "HEAD" => reqwest::Method::HEAD,
            "OPTIONS" => reqwest::Method::OPTIONS,
            "PATCH" => reqwest::Method::PATCH,
            _ => reqwest::Method::GET,
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
                println!("Received response with status: {}", response.status());
                match response.bytes().await {
                    Ok(body_bytes) => HttpProxyResponse {
                        status_code,
                        headers,
                        body: body_bytes.to_vec(),
                        error: None,
                    },
                    Err(e) => {
                        eprintln!("HTTP request failed: {}", e);
                        HttpProxyResponse {
                            status_code: 500,
                            headers: HashMap::new(),
                            body: Vec::new(),
                            error: Some(format!("Failed to read response body: {}", e)),
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("HTTP request failed: {}", e);
                HttpProxyResponse {
                    status_code: 500,
                    headers: HashMap::new(),
                    body: Vec::new(),
                    error: Some(format!("HTTP request failed: {}", e)),
                }
            }
        }
    })
}

// Relay data in both directions between UnixStream and TcpStream for CONNECT tunneling
fn relay_bidirectional(
    stream1: &mut UnixStream,
    stream2: &mut TcpStream,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut s1a = stream1.try_clone()?;
    let mut s1b = stream1.try_clone()?;
    let mut s2a = stream2.try_clone()?;
    let mut s2b = stream2.try_clone()?;

    let closed = Arc::new(AtomicBool::new(false));
    let closed1 = closed.clone();
    let closed2 = closed.clone();

    // Client -> Server
    let s2a_shutdown = s2a.try_clone()?;
    thread::spawn(move || {
        let res = std::io::copy(&mut s1a, &mut s2a);
        println!("Client->Server relay thread exiting, result: {:?}", res);
        if !closed1.swap(true, Ordering::SeqCst) {
            let _ = s2a_shutdown.shutdown(Shutdown::Write);
        }
    });

    // Server -> Client
    let s1b_shutdown = s1b.try_clone()?;
    thread::spawn(move || {
        let res = std::io::copy(&mut s2b, &mut s1b);
        println!("Server->Client relay thread exiting, result: {:?}", res);
        if !closed2.swap(true, Ordering::SeqCst) {
            let _ = s1b_shutdown.shutdown(Shutdown::Write);
        }
    });

    // Do not join the threads; return immediately to avoid blocking the main proxy loop
    Ok(())
}
