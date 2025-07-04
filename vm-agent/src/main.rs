use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use vsock::VsockStream;
use tokio::sync::Mutex;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode, Uri};
use std::convert::Infallible;

// Simple command structure expected by the host
#[derive(Debug, Serialize, Deserialize)]
struct CommandRequest {
    command: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CommandResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

// HTTP Proxy structures
#[derive(Debug, Serialize, Deserialize)]
struct HttpProxyRequest {
    method: String,
    url: String,
    headers: HashMap<String, String>,
    body: Option<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct HttpProxyResponse {
    status_code: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
    error: Option<String>,
}

// Unified request/response for VSOCK communication
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum VsockRequest {
    Command(CommandRequest),
    HttpProxy(HttpProxyRequest),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum VsockResponse {
    Command(CommandResponse),
    HttpProxy(HttpProxyResponse),
}

fn execute_command(command: &str) -> CommandResponse {
    println!("Executing command: {}", command);

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

            println!("Command completed with exit code {}", exit_code);

            CommandResponse {
                exit_code,
                stdout,
                stderr,
            }
        }
        Err(e) => {
            eprintln!("Failed to execute command: {}", e);
            CommandResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Failed to execute command: {}", e),
            }
        }
    }
}

fn handle_connection(mut stream: vsock::VsockStream) -> Result<(), Box<dyn std::error::Error>> {
    println!("=== NEW CONNECTION HANDLER STARTED ===");

    // Remove the read timeout to handle non-blocking operations manually
    match stream.set_read_timeout(None) {
        Ok(_) => println!("Read timeout disabled successfully"),
        Err(e) => {
            eprintln!("Failed to set read timeout: {}", e);
            return Err(e.into());
        }
    }

    let mut buffer = [0; 4096];
    let mut total_message = String::new();
    let read_timeout = std::time::Duration::from_secs(10);
    let start_time = std::time::Instant::now();
    let mut read_attempts = 0;
    let mut response_sent = false;

    println!("Starting read loop...");

    // Loop to handle partial reads and WouldBlock errors
    loop {
        read_attempts += 1;
        println!("Read attempt #{}", read_attempts);

        match stream.read(&mut buffer) {
            Ok(0) => {
                println!("Connection closed by client (read returned 0)");
                break;
            }
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buffer[..n]);
                total_message.push_str(&chunk);
                println!(
                    "SUCCESS: Received {} bytes, total: {} bytes",
                    n,
                    total_message.len()
                );
                println!("Received chunk: '{}'", chunk);
                println!("Total message so far: '{}'", total_message);

                // Try to parse as complete JSON
                println!("Attempting to parse JSON...");

                // First try to parse as new VsockRequest format
                if let Ok(request) = serde_json::from_str::<VsockRequest>(&total_message) {
                    println!("SUCCESS: JSON parsed as VsockRequest");
                    let response = match request {
                        VsockRequest::Command(cmd_req) => {
                            println!("Executing command: '{}'", cmd_req.command);
                            let cmd_response = execute_command(&cmd_req.command);
                            VsockResponse::Command(cmd_response)
                        }
                        VsockRequest::HttpProxy(proxy_req) => {
                            println!(
                                "Processing HTTP proxy request: {} {}",
                                proxy_req.method, proxy_req.url
                            );
                            // For now, return an error since we need the host to handle this
                            let error_response = HttpProxyResponse {
                                status_code: 500,
                                headers: HashMap::new(),
                                body: b"HTTP proxy not yet implemented in VM agent".to_vec(),
                                error: Some("HTTP proxy functionality requires host-side implementation".to_string()),
                            };
                            VsockResponse::HttpProxy(error_response)
                        }
                    };
                    let response_json = serde_json::to_string(&response)?;

                        println!("Sending response: {}", response_json);
                        match stream.write_all(response_json.as_bytes()) {
                            Ok(_) => {
                                println!("Response written to stream");
                                match stream.flush() {
                                    Ok(_) => {
                                        println!("Response flushed successfully");
                                        // Don't wait - let the connection close naturally
                                        // The host will detect the connection closure and parse the complete response
                                        println!("Connection handler will now close");
                                    }
                                    Err(e) => eprintln!("Failed to flush response: {}", e),
                                }
                            }
                            Err(e) => eprintln!("Failed to send response: {}", e),
                        }
                        response_sent = true;
                        break;
                }
                // If VsockRequest parsing fails, try old CommandRequest format for backward compatibility
                else if let Ok(cmd_request) = serde_json::from_str::<CommandRequest>(&total_message) {
                    println!("SUCCESS: JSON parsed as legacy CommandRequest");
                    println!("Executing command: '{}'", cmd_request.command);
                    let cmd_response = execute_command(&cmd_request.command);
                    let response_json = serde_json::to_string(&cmd_response)?;

                    println!("Sending response: {}", response_json);
                    match stream.write_all(response_json.as_bytes()) {
                        Ok(_) => {
                            println!("Response written to stream");
                            match stream.flush() {
                                Ok(_) => {
                                    println!("Response flushed successfully");
                                    println!("Connection handler will now close");
                                }
                                Err(e) => eprintln!("Failed to flush response: {}", e),
                            }
                        }
                        Err(e) => eprintln!("Failed to send response: {}", e),
                    }
                    response_sent = true;
                    break;
                } else {
                    println!("JSON parse failed for both formats - continuing to read more data");
                }

                // Reset buffer for next read
                buffer = [0; 4096];
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                println!(
                    "WouldBlock error - no data available yet (elapsed: {:?})",
                    start_time.elapsed()
                );
                if start_time.elapsed() > read_timeout {
                    println!("TIMEOUT: Read timeout reached, sending error response");
                    let error_response = CommandResponse {
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: "Read timeout waiting for complete command".to_string(),
                    };
                    let response_json = serde_json::to_string(&error_response)?;
                    let _ = stream.write_all(response_json.as_bytes());
                    let _ = stream.flush();
                    break;
                }
                // Wait a bit before trying again
                println!("Sleeping 50ms before next read attempt...");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                println!("ERROR: Read error - {} (kind: {:?})", e, e.kind());
                // Send an error response if possible
                let error_response = CommandResponse {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("Read error: {}", e),
                };
                if let Ok(response_json) = serde_json::to_string(&error_response) {
                    let _ = stream.write_all(response_json.as_bytes());
                    let _ = stream.flush();
                }
                break;
            }
        }
    }

    // If we accumulated data but couldn't parse it as JSON, send error (only if no response was sent)
    if !response_sent && !total_message.is_empty() && !total_message.trim().is_empty() {
        if serde_json::from_str::<VsockRequest>(&total_message).is_err()
            && serde_json::from_str::<CommandRequest>(&total_message).is_err() {
            println!(
                "FINAL ERROR: Failed to parse accumulated JSON: '{}'",
                total_message
            );
            // Try to send error as legacy format first (more likely to work)
            let error_response = CommandResponse {
                exit_code: -1,
                stdout: String::new(),
                stderr: format!("Invalid JSON request: {}", total_message),
            };
            if let Ok(response_json) = serde_json::to_string(&error_response) {
                let _ = stream.write_all(response_json.as_bytes());
                let _ = stream.flush();
            }
        }
    }

    println!("=== CONNECTION HANDLER FINISHED ===");
    Ok(())
}

// HTTP proxy client for making requests to host
struct VsockHttpClient {}

impl VsockHttpClient {
    fn new() -> Self {
        Self {}
    }

    async fn make_request(&self, req: HttpProxyRequest) -> Result<HttpProxyResponse, Box<dyn std::error::Error + Send + Sync>> {
        // Use spawn_blocking to handle synchronous VSOCK operations
        let result = tokio::task::spawn_blocking(move || {
            // Create a new connection for each request
            let mut stream = vsock::VsockStream::connect_with_cid_port(vsock::VMADDR_CID_HOST, 1235)?;

            // Send request
            let vsock_request = VsockRequest::HttpProxy(req);
            let request_json = serde_json::to_string(&vsock_request)?;
            stream.write_all(request_json.as_bytes())?;
            stream.flush()?;

            // Read response
            let mut buffer = [0; 8192];
            let mut response_data = String::new();

            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break, // Connection closed
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buffer[..n]);
                        response_data.push_str(&chunk);

                        // Try to parse complete response
                        if let Ok(vsock_response) = serde_json::from_str::<VsockResponse>(&response_data) {
                            if let VsockResponse::HttpProxy(proxy_response) = vsock_response {
                                return Ok(proxy_response);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                }
            }

            Err("Failed to get response from host".into())
        }).await;

        match result {
            Ok(response) => response,
            Err(e) => Err(format!("Task join error: {}", e).into()),
        }
    }
}

async fn handle_http_request(
    req: Request<Body>,
    client: Arc<VsockHttpClient>,
) -> Result<Response<Body>, Infallible> {
    // Handle CONNECT method for HTTPS proxy
    if req.method() == hyper::Method::CONNECT {
        // For CONNECT requests, the URI is the target host:port
        let target = req.uri().to_string();
        println!("CONNECT request to: {}", target);

        // Return 200 Connection Established for now
        // In a full implementation, you'd establish a tunnel
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Body::from("Connection established"))
            .unwrap());
    }

    let method = req.method().to_string();

    // For standard HTTP proxy, extract target URL from request
    let target_url = if req.uri().scheme().is_some() {
        // Absolute URL (standard proxy format)
        req.uri().to_string()
    } else {
        // Relative URL - this shouldn't happen in proxy mode
        format!("http://{}{}",
            req.headers().get("host")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost"),
            req.uri().path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/")
        )
    };

    println!("Proxying {} request to: {}", method, target_url);

    // Collect headers (excluding proxy-specific headers)
    let mut headers = HashMap::new();
    for (name, value) in req.headers().iter() {
        let name_str = name.as_str().to_lowercase();
        if !name_str.starts_with("proxy-") {
            if let Ok(value_str) = value.to_str() {
                headers.insert(name.to_string(), value_str.to_string());
            }
        }
    }

    // Get body
    let body_bytes = match hyper::body::to_bytes(req.into_body()).await {
        Ok(bytes) => {
            if bytes.is_empty() {
                None
            } else {
                Some(bytes.to_vec())
            }
        }
        Err(_) => None,
    };

    let proxy_request = HttpProxyRequest {
        method,
        url: target_url,
        headers,
        body: body_bytes,
    };

    match client.make_request(proxy_request).await {
        Ok(proxy_response) => {
            let mut response_builder = Response::builder()
                .status(proxy_response.status_code);

            // Add headers
            for (name, value) in proxy_response.headers {
                response_builder = response_builder.header(&name, &value);
            }

            match response_builder.body(Body::from(proxy_response.body)) {
                Ok(response) => Ok(response),
                Err(_) => Ok(Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("Failed to build response"))
                    .unwrap()),
            }
        }
        Err(e) => {
            println!("Proxy request failed: {}", e);
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("Proxy error: {}", e)))
                .unwrap())
        }
    }
}

async fn start_http_proxy_server() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = Arc::new(VsockHttpClient::new());

    let make_svc = make_service_fn(move |_conn| {
        let client = client.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                handle_http_request(req, client.clone())
            }))
        }
    });

    // Bind to all interfaces on port 8080 to act as HTTP proxy
    let addr = ([0, 0, 0, 0], 8080).into();
    let server = Server::bind(&addr).serve(make_svc);

    println!("HTTP Proxy Server listening on 0.0.0.0:8080");
    println!("Set http_proxy=http://127.0.0.1:8080 to use this proxy");

    server.await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== VM AGENT STARTING ===");
    println!("Starting VM Agent with VSOCK server on port 1234 and HTTP proxy on port 8080");

    // Start HTTP proxy server in background
    let proxy_handle = tokio::spawn(async {
        if let Err(e) = start_http_proxy_server().await {
            eprintln!("HTTP proxy server error: {}", e);
        }
    });

    // Give the HTTP proxy server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Start VSOCK server in a separate task
    let vsock_handle = tokio::task::spawn_blocking(|| {
        // Check system information
        println!("Checking system capabilities...");
        if std::path::Path::new("/dev/vsock").exists() {
            println!("✓ VSOCK device found at /dev/vsock");
        } else {
            println!("⚠ WARNING: VSOCK device not found at /dev/vsock");
        }

        // Check what VSOCK modules are actually loaded
        println!("Checking loaded VSOCK modules...");
        if let Ok(output) = std::process::Command::new("lsmod").output() {
            let lsmod_output = String::from_utf8_lossy(&output.stdout);
            if lsmod_output.contains("vsock") {
                println!("✓ VSOCK modules are loaded:");
                for line in lsmod_output.lines() {
                    if line.contains("vsock") {
                        println!("  {}", line);
                    }
                }
            } else {
                println!("⚠ No VSOCK modules found in lsmod output");
            }
        }

        // Try to load VSOCK modules
        println!("Attempting to load VSOCK modules...");
        if let Ok(output) = std::process::Command::new("modprobe").arg("vsock").output() {
            println!(
                "modprobe vsock result: {} (stderr: {})",
                output.status.success(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if let Ok(output) = std::process::Command::new("modprobe")
            .arg("vmw_vsock_virtio_transport")
            .output()
        {
            println!(
                "modprobe vmw_vsock_virtio_transport result: {} (stderr: {})",
                output.status.success(),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Attempt to bind VSOCK listener
        println!("Attempting to bind VSOCK listener...");
        match vsock::VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, 1234) {
            Ok(listener) => {
                println!("✓ VSOCK listener bound successfully on port 1234");
                println!("Entering connection accept loop...");

                let mut connection_count = 0;
                for stream in listener.incoming() {
                    connection_count += 1;
                    println!(">>> INCOMING CONNECTION #{} <<<", connection_count);
                    match stream {
                        Ok(stream) => {
                            println!(
                                "✓ New VSOCK connection accepted (connection #{})",
                                connection_count
                            );
                            // Handle each connection in current thread for easier debugging
                            if let Err(e) = handle_connection(stream) {
                                eprintln!("✗ Error handling connection #{}: {}", connection_count, e);
                            }
                            println!(
                                "Connection #{} handling completed, waiting for next connection...",
                                connection_count
                            );
                        }
                        Err(e) => {
                            eprintln!("✗ Error accepting connection #{}: {}", connection_count, e);
                        }
                    }
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("✗ FAILED to bind VSOCK listener on port 1234: {}", e);
                Err(e)
            }
        }
    });

    // Wait for either task to complete
    tokio::select! {
        proxy_result = proxy_handle => {
            match proxy_result {
                Ok(_) => println!("HTTP proxy server completed"),
                Err(e) => eprintln!("HTTP proxy server task error: {}", e),
            }
        }
        vsock_result = vsock_handle => {
            match vsock_result {
                Ok(Ok(_)) => println!("VSOCK server completed"),
                Ok(Err(e)) => {
                    eprintln!("VSOCK server error: {}", e);
                    return Err(e.into());
                }
                Err(e) => {
                    eprintln!("VSOCK server task error: {}", e);
                    return Err(e.into());
                }
            }
        }
    }

    println!("=== VM AGENT EXITING ===");
    Ok(())
}
