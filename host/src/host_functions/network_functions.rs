use std::sync::Arc;

use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

pub fn http_request(
    client: Arc<Client>,
    url: &str,
    method: &str,
    body: Option<&[u8]>,
    headers: Option<&[(&str, &str)]>,
) -> Result<String, Box<dyn std::error::Error>> {
    let method = match method.to_uppercase().as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PUT" => Method::PUT,
        "DELETE" => Method::DELETE,
        "HEAD" => Method::HEAD,
        "OPTIONS" => Method::OPTIONS,
        "PATCH" => Method::PATCH,
        _ => return Err("Invalid HTTP method".into()),
    };

    let mut request_builder = client.request(method, url);

    if let Some(body_data) = body {
        request_builder = request_builder.body(body_data.to_vec());
    }

    // Build headers, add a fallback User-Agent if not provided
    let mut header_map = HeaderMap::new();
    let mut user_agent_set = false;

    if let Some(header_pairs) = headers {
        for (key, value) in header_pairs {
            let name = HeaderName::from_bytes(key.as_bytes())?;
            let val = HeaderValue::from_str(value)?;
            if name == reqwest::header::USER_AGENT {
                user_agent_set = true;
            }
            header_map.insert(name, val);
        }
    }

    // Fallback User-Agent
    if !user_agent_set {
        header_map.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_static("curl/7.64.1"),
        );
    }

    request_builder = request_builder.headers(header_map);

    let response = request_builder.send()?;

    let body_text = response.text()?;
    Ok(body_text)
}
