use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::io::Write;

mod logger;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
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
    log::info!("Executing command: {}", command);

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

            log::info!("Command completed with exit code {}", exit_code);

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

fn handle_connection(mut stream: vsock::VsockStream) -> Result<(), Box<dyn std::error::Error>> {
    log::info!("=== NEW CONNECTION HANDLER STARTED ===");

    // Remove the read timeout to handle non-blocking operations manually
    match stream.set_read_timeout(None) {
        Ok(_) => log::info!("Read timeout disabled successfully"),
        Err(e) => {
            log::error!("Failed to set read timeout: {}", e);
            return Err(e.into());
        }
    }

    let mut buffer = [0; 4096];
    let mut total_message = String::new();
    let read_timeout = std::time::Duration::from_secs(10);
    let start_time = std::time::Instant::now();
    let mut read_attempts = 0;
    let mut response_sent = false;

    log::info!("Starting read loop...");

    // Loop to handle partial reads and WouldBlock errors
    loop {
        read_attempts += 1;
        log::info!("Read attempt #{}", read_attempts);

        match stream.read(&mut buffer) {
            Ok(0) => {
                log::info!("Connection closed by client (read returned 0)");
                break;
            }
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buffer[..n]);
                total_message.push_str(&chunk);
                log::info!(
                    "SUCCESS: Received {} bytes, total: {} bytes",
                    n,
                    total_message.len()
                );
                log::info!("Received chunk: '{}'", chunk);
                log::info!("Total message so far: '{}'", total_message);

                // Try to parse as complete JSON
                log::info!("Attempting to parse JSON...");

                // First try to parse as new VsockRequest format
                if let Ok(request) = serde_json::from_str::<VsockRequest>(&total_message) {
                    log::info!("SUCCESS: JSON parsed as VsockRequest");
                    let response = match request {
                        VsockRequest::Command(cmd_req) => {
                            log::info!("Executing command: '{}'", cmd_req.command);
                            let cmd_response = execute_command(&cmd_req.command);
                            VsockResponse::Command(cmd_response)
                        }
                        VsockRequest::HttpProxy(proxy_req) => {
                            log::info!(
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

                        log::info!("Sending response: {}", response_json);
                        match stream.write_all(response_json.as_bytes()) {
                            Ok(_) => {
                                log::info!("Response written to stream");
                                match stream.flush() {
                                    Ok(_) => {
                                        log::info!("Response flushed successfully");
                                        // Don't wait - let the connection close naturally
                                        // The host will detect the connection closure and parse the complete response
                                        log::info!("Connection handler will now close");
                                    }
                                    Err(e) => log::error!("Failed to flush response: {}", e),
                                }
                            }
                            Err(e) => log::error!("Failed to send response: {}", e),
                        }
                        response_sent = true;
                        break;
                }
                // If VsockRequest parsing fails, try old CommandRequest format for backward compatibility
                else if let Ok(cmd_request) = serde_json::from_str::<CommandRequest>(&total_message) {
                    log::info!("SUCCESS: JSON parsed as legacy CommandRequest");
                    log::info!("Executing command: '{}'", cmd_request.command);
                    let cmd_response = execute_command(&cmd_request.command);
                    let response_json = serde_json::to_string(&cmd_response)?;

                    log::info!("Sending response: {}", response_json);
                    match stream.write_all(response_json.as_bytes()) {
                        Ok(_) => {
                            log::info!("Response written to stream");
                            match stream.flush() {
                                Ok(_) => {
                                    log::info!("Response flushed successfully");
                                    log::info!("Connection handler will now close");
                                }
                                Err(e) => log::error!("Failed to flush response: {}", e),
                            }
                        }
                        Err(e) => log::error!("Failed to send response: {}", e),
                    }
                    response_sent = true;
                    break;
                } else {
                    log::info!("JSON parse failed for both formats - continuing to read more data");
                }

                // Reset buffer for next read
                buffer = [0; 4096];
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                log::info!(
                    "WouldBlock error - no data available yet (elapsed: {:?})",
                    start_time.elapsed()
                );
                if start_time.elapsed() > read_timeout {
                    log::info!("TIMEOUT: Read timeout reached, sending error response");
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
                log::info!("Sleeping 50ms before next read attempt...");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                log::info!("ERROR: Read error - {} (kind: {:?})", e, e.kind());
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
            log::info!(
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

    log::info!("=== CONNECTION HANDLER FINISHED ===");
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
        log::info!("CONNECT request to: {}", target);
        log::info!("CONNECT method received. Target: {}", target);
        let vsock_port = 1235; // Replace with the actual vsock port the host proxy listens on

        // Establish a vsock connection to the host proxy
        log::info!("Attempting to establish a vsock connection to the host proxy at CID: {}, Port: {}", vsock::VMADDR_CID_HOST, vsock_port);
        match vsock::VsockStream::connect_with_cid_port(vsock::VMADDR_CID_HOST, vsock_port) {
            Ok(mut host_stream) => {
                log::info!("Successfully established a vsock connection to the host proxy at CID: {}, Port: {}", vsock::VMADDR_CID_HOST, vsock_port);

                // Send the actual CONNECT request line to the host proxy
                let connect_line = format!("CONNECT {} HTTP/1.1\r\n\r\n", target);
                if let Err(e) = host_stream.write_all(connect_line.as_bytes()) {
                    log::error!("Failed to send CONNECT request to host proxy: {}", e);
                } else {
                    log::info!("CONNECT request sent to host proxy. Waiting for response...");
                    let mut response_buffer = [0u8; 1024];
                    match host_stream.read(&mut response_buffer) {
                        Ok(n) if n > 0 => {
                            log::info!("Received response from host proxy: {:?}", &response_buffer[..n]);
                        }
                        Ok(_) => {
                            log::warn!("Host proxy closed connection without responding to CONNECT request.");
                        }
                        Err(e) => {
                            log::error!("Failed to read response from host proxy: {}", e);
                        }
                    }
                }

                // Spawn a background task to handle the upgrade and forwarding
                let req_for_upgrade = req;
                tokio::spawn(async move {
                    log::info!("Trying to upgrade connection (background task)");
                    let upgraded = hyper::upgrade::on(req_for_upgrade).await;
                    match upgraded {
                        Ok(upgraded) => {
                            log::info!("Upgrade succeeded, starting forwarding tasks.");
                            let (mut client_reader, mut client_writer) = tokio::io::split(upgraded);

                            // The vsock stream is sync, so we need to handle it in blocking threads.
                            // We'll use channels to pass data between the async and sync worlds.
                            log::info!("Cloning the VSOCK Stream");
                            let mut host_reader = host_stream.try_clone().expect("Failed to clone vsock stream for reading");
                            let mut host_writer = host_stream;

                            let (c2h_tx, mut c2h_rx) = mpsc::channel::<Vec<u8>>(2);
                            let (h2c_tx, mut h2c_rx) = mpsc::channel::<Vec<u8>>(2);

                            log::info!("Starting packet forwarding between client and host proxy...");

                            // 1. Read from async client, send to channel
                            log::info!("Spawning client_reader_task");
                            let client_reader_task = tokio::spawn(async move {
                                log::info!("Client -> Host: Reader task started.");
                                loop {
                                    let mut buf = vec![0u8; 4096];
                                    log::info!("Client -> Host: Waiting to read data from client...");
                                    match client_reader.read(&mut buf).await {
                                        Ok(0) => {
                                            log::info!("Client -> Host: Client closed connection.");
                                            break;
                                        }
                                        Ok(n) => {
                                            buf.truncate(n);
                                            log::info!("Client -> Host: Read {} bytes from client. Data: {:?}", n, &buf[..n.min(100)]);
                                            if c2h_tx.send(buf).await.is_err() {
                                                log::error!("Client -> Host: Failed to send data to host channel.");
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            log::error!("Client -> Host: Error reading from client: {}", e);
                                            break;
                                        }
                                    }
                                }
                                log::info!("Client -> Host: Reader task ended.");
                            });

                            // 2. Receive from channel, write to sync host
                            log::info!("Spawning client_writer_task");
                            let client_writer_task = tokio::task::spawn_blocking(move || {
                                while let Some(data) = c2h_rx.blocking_recv() {
                                    log::info!("Client -> Host: Writing {} bytes to host. Data: {:?}", data.len(), &data[..data.len().min(100)]);
                                    if host_writer.write_all(&data).is_err() {
                                        log::error!("Client -> Host: Failed to write data to host.");
                                        break;
                                    }
                                }
                            });

                            // 3. Read from sync host, send to channel
                            let host_reader_task = tokio::task::spawn_blocking(move || {
                                loop {
                                    let mut buf = vec![0u8; 4096];
                                    log::info!("Host -> Client: Waiting to read data from host...");
                                    match host_reader.read(&mut buf) {
                                        Ok(0) => {
                                            log::info!("Host -> Client: Host closed connection.");
                                            break;
                                        }
                                        Ok(n) => {
                                            buf.truncate(n);
                                            log::info!("Host -> Client: Read {} bytes from host. Data: {:?}", n, &buf[..n.min(100)]);
                                            // Use blocking_send as we are in a sync context.
                                            if h2c_tx.blocking_send(buf).is_err() {
                                                log::error!("Host -> Client: Failed to send data to client channel.");
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            log::error!("Host -> Client: Error reading from host: {}", e);
                                            break;
                                        }
                                    }
                                }
                            });

                            // 4. Receive from channel, write to async client
                            let host_writer_task = tokio::spawn(async move {
                                log::info!("Host -> Client: Writer task started.");
                                while let Some(data) = h2c_rx.recv().await {
                                    log::info!("Host -> Client: Writing {} bytes to client. Data: {:?}", data.len(), &data[..data.len().min(100)]);
                                    if client_writer.write_all(&data).await.is_err() {
                                        log::error!("Host -> Client: Failed to write data to client.");
                                        break;
                                    }
                                }
                                log::info!("Host -> Client: Writer task ended.");
                            });

                            // Wait for any of the tasks to finish, which indicates the connection is closing.
                            tokio::select! {
                                _ = client_reader_task => log::info!("Client reader task finished."),
                                _ = client_writer_task => log::info!("Client writer task finished."),
                                _ = host_reader_task => log::info!("Host reader task finished."),
                                _ = host_writer_task => log::info!("Host writer task finished."),
                            }
                            log::info!("Packet forwarding terminated.");
                            log::info!("CONNECT request handling completed successfully.");
                        }
                        Err(e) => {
                            log::error!("Upgrade error: {}", e);
                        }
                    }
                });

                // Immediately return the 200 Connection Established response to the client
                return Ok(
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Body::empty())
                        .unwrap()
                );
            }
            Err(e) => {
                log::error!("Failed to connect to host proxy: {}", e);
                return Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::from("Failed to connect to host proxy"))
                    .unwrap());
            }
        }
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

    log::info!("Proxying {} request to: {}", method, target_url);

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
            log::info!("Proxy request failed: {}", e);
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

    log::info!("HTTP Proxy Server listening on 0.0.0.0:8080");
    log::info!("Set http_proxy=http://127.0.0.1:8080 to use this proxy");

    server.await?;
    Ok(())
}



// Old VsockLogger removed; replaced by logger::bounded_logger

// Old CombinedLogger and init_combined_logger removed; replaced by logger::bounded_logger

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize the bounded vsock logger and combined logger with background task
    let vsock_logger = logger::bounded_logger::BoundedVsockLogger::init(1236).await;
    logger::bounded_logger::init_combined_logger(vsock_logger.clone()).expect("Failed to initialize logger");

    log::info!("=== VM AGENT STARTING ===");
    log::info!("Starting VM Agent with VSOCK server on port 1234 and HTTP proxy on port 8080");

    // Start HTTP proxy server in background
    let proxy_handle = tokio::spawn(async {
        if let Err(e) = start_http_proxy_server().await {
            log::error!("HTTP proxy server error: {}", e);
        }
    });

    // Give the HTTP proxy server time to start
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Start VSOCK server in a separate task
    let vsock_handle = tokio::task::spawn_blocking(|| {
        // Check system information
        log::info!("Checking system capabilities...");
        if std::path::Path::new("/dev/vsock").exists() {
            log::info!("✓ VSOCK device found at /dev/vsock");
        } else {
            log::info!("⚠ WARNING: VSOCK device not found at /dev/vsock");
        }

        // Check what VSOCK modules are actually loaded
        log::info!("Checking loaded VSOCK modules...");
        if let Ok(output) = std::process::Command::new("lsmod").output() {
            let lsmod_output = String::from_utf8_lossy(&output.stdout);
            if lsmod_output.contains("vsock") {
                log::info!("✓ VSOCK modules are loaded:");
                for line in lsmod_output.lines() {
                    if line.contains("vsock") {
                        log::info!("  {}", line);
                    }
                }
            } else {
                log::info!("⚠ No VSOCK modules found in lsmod output");
            }
        }

        // Try to load VSOCK modules
        log::info!("Attempting to load VSOCK modules...");
        if let Ok(output) = std::process::Command::new("modprobe").arg("vsock").output() {
            log::info!(
                "modprobe vsock result: {} (stderr: {})",
                output.status.success(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if let Ok(output) = std::process::Command::new("modprobe")
            .arg("vmw_vsock_virtio_transport")
            .output()
        {
            log::info!(
                "modprobe vmw_vsock_virtio_transport result: {} (stderr: {})",
                output.status.success(),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Attempt to bind VSOCK listener
        log::info!("Attempting to bind VSOCK listener...");
        match vsock::VsockListener::bind_with_cid_port(vsock::VMADDR_CID_ANY, 1234) {
            Ok(listener) => {
                log::info!("✓ VSOCK listener bound successfully on port 1234");
                log::info!("Entering connection accept loop...");

                let mut connection_count = 0;
                for stream in listener.incoming() {
                    connection_count += 1;
                    log::info!(">>> INCOMING CONNECTION #{} <<<", connection_count);
                    match stream {
                        Ok(stream) => {
                            log::info!(
                                "✓ New VSOCK connection accepted (connection #{})",
                                connection_count
                            );
                            // Handle each connection in current thread for easier debugging
                            if let Err(e) = handle_connection(stream) {
                                log::error!("✗ Error handling connection #{}: {}", connection_count, e);
                            }
                            log::info!(
                                "Connection #{} handling completed, waiting for next connection...",
                                connection_count
                            );
                        }
                        Err(e) => {
                            log::error!("✗ Error accepting connection #{}: {}", connection_count, e);
                        }
                    }
                }
                Ok(())
            }
            Err(e) => {
                log::error!("✗ FAILED to bind VSOCK listener on port 1234: {}", e);
                Err(e)
            }
        }
    });

    // Wait for either task to complete
    tokio::select! {
        proxy_result = proxy_handle => {
            match proxy_result {
                Ok(_) => log::info!("HTTP proxy server completed"),
                Err(e) => log::error!("HTTP proxy server task error: {}", e),
            }
        }
        vsock_result = vsock_handle => {
            match vsock_result {
                Ok(Ok(_)) => log::info!("VSOCK server completed"),
                Ok(Err(e)) => {
                    log::error!("VSOCK server error: {}", e);
                    return Err(e.into());
                }
                Err(e) => {
                    log::error!("VSOCK server task error: {}", e);
                    return Err(e.into());
                }
            }
        }
    }
    Ok(())
}
