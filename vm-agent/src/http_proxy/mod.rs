use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::io::{Read, Write};
use crate::{VsockRequest, VsockResponse};

use hyper::{Body, Request, Response, Server, StatusCode};
use hyper::service::{make_service_fn, service_fn};
use tokio::sync::mpsc;
use serde::{Serialize, Deserialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Request struct for HTTP proxying over vsock
#[derive(Debug, Serialize, Deserialize)]
pub struct HttpProxyRequest {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    pub body: Option<Vec<u8>>,
}

/// Response struct for HTTP proxying over vsock
#[derive(Debug, Serialize, Deserialize)]
pub struct HttpProxyResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub error: Option<String>,
}



/// HTTP client for vsock proxying
pub struct VsockHttpClient {}

impl VsockHttpClient {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn make_request(
        &self,
        req: HttpProxyRequest,
    ) -> Result<HttpProxyResponse, Box<dyn std::error::Error + Send + Sync>> {
        let result = tokio::task::spawn_blocking(move || {
            let mut stream = vsock::VsockStream::connect_with_cid_port(vsock::VMADDR_CID_HOST, 1235)?;

            let vsock_request = VsockRequest::HttpProxy(req);
            let request_json = serde_json::to_string(&vsock_request)?;
            stream.write_all(request_json.as_bytes())?;
            stream.flush()?;

            let mut buffer = [0; 8192];
            let mut response_data = String::new();

            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buffer[..n]);
                        response_data.push_str(&chunk);

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

/// Handles incoming HTTP requests and proxies them over vsock
pub async fn handle_http_request(
    req: Request<Body>,
    client: Arc<VsockHttpClient>,
) -> Result<Response<Body>, Infallible> {
    if req.method() == hyper::Method::CONNECT {
        let target = req.uri().to_string();
        log::debug!("CONNECT request to: {}", target);
        let vsock_port = 1235;

        log::debug!("Attempting to establish a vsock connection to the host proxy at CID: {}, Port: {}", vsock::VMADDR_CID_HOST, vsock_port);
        match vsock::VsockStream::connect_with_cid_port(vsock::VMADDR_CID_HOST, vsock_port) {
            Ok(mut host_stream) => {
                let connect_line = format!("CONNECT {} HTTP/1.1\r\n\r\n", target);
                if let Err(e) = host_stream.write_all(connect_line.as_bytes()) {
                    log::error!("Failed to send CONNECT request to host proxy: {}", e);
                } else {
                    let mut response_buffer = [0u8; 1024];
                    match host_stream.read(&mut response_buffer) {
                        Ok(n) if n > 0 => {
                            log::debug!("Received response from host proxy: {:?}", &response_buffer[..n]);
                        }
                        Ok(_) => {
                            log::warn!("Host proxy closed connection without responding to CONNECT request.");
                        }
                        Err(e) => {
                            log::error!("Failed to read response from host proxy: {}", e);
                        }
                    }
                }

                let req_for_upgrade = req;
                tokio::spawn(async move {
                    let upgraded = hyper::upgrade::on(req_for_upgrade).await;
                    match upgraded {
                        Ok(upgraded) => {
                            let (mut client_reader, mut client_writer) = tokio::io::split(upgraded);

                            let mut host_reader = host_stream.try_clone().expect("Failed to clone vsock stream for reading");
                            let mut host_writer = host_stream;

                            let (c2h_tx, mut c2h_rx) = mpsc::channel::<Vec<u8>>(2);
                            let (h2c_tx, mut h2c_rx) = mpsc::channel::<Vec<u8>>(2);

                            let client_reader_task = tokio::spawn(async move {
                                loop {
                                    let mut buf = vec![0u8; 4096];
                                    match client_reader.read(&mut buf).await {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            buf.truncate(n);
                                            if c2h_tx.send(buf).await.is_err() {
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                            });

                            let client_writer_task = tokio::task::spawn_blocking(move || {
                                while let Some(data) = c2h_rx.blocking_recv() {
                                    if host_writer.write_all(&data).is_err() {
                                        break;
                                    }
                                }
                            });

                            let host_reader_task = tokio::task::spawn_blocking(move || {
                                loop {
                                    let mut buf = vec![0u8; 4096];
                                    match host_reader.read(&mut buf) {
                                        Ok(0) => break,
                                        Ok(n) => {
                                            buf.truncate(n);
                                            if h2c_tx.blocking_send(buf).is_err() {
                                                break;
                                            }
                                        }
                                        Err(_) => break,
                                    }
                                }
                            });

                            let host_writer_task = tokio::spawn(async move {
                                while let Some(data) = h2c_rx.recv().await {
                                    if client_writer.write_all(&data).await.is_err() {
                                        break;
                                    }
                                }
                            });

                            tokio::select! {
                                _ = client_reader_task => {},
                                _ = client_writer_task => {},
                                _ = host_reader_task => {},
                                _ = host_writer_task => {},
                            }
                        }
                        Err(e) => {
                            log::error!("Upgrade error: {}", e);
                        }
                    }
                });

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

    let target_url = if req.uri().scheme().is_some() {
        req.uri().to_string()
    } else {
        format!("http://{}{}",
            req.headers().get("host")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost"),
            req.uri().path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/")
        )
    };

    let mut headers = HashMap::new();
    for (name, value) in req.headers().iter() {
        let name_str = name.as_str().to_lowercase();
        if !name_str.starts_with("proxy-") {
            if let Ok(value_str) = value.to_str() {
                headers.insert(name.to_string(), value_str.to_string());
            }
        }
    }

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
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("Proxy error: {}", e)))
                .unwrap())
        }
    }
}

/// Starts the HTTP proxy server
pub async fn start_http_proxy_server() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = Arc::new(VsockHttpClient::new());

    let make_svc = make_service_fn(move |_conn| {
        let client = client.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                handle_http_request(req, client.clone())
            }))
        }
    });

    let addr = ([0, 0, 0, 0], 8080).into();
    let server = Server::bind(&addr).serve(make_svc);

    log::info!("HTTP Proxy Server listening on 0.0.0.0:8080");
    log::info!("Set http_proxy=http://127.0.0.1:8080 to use this proxy");

    server.await?;
    Ok(())
}
