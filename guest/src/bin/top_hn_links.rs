#![no_std]
#![no_main]

extern crate alloc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format};
use regex::Regex;
use hyperlight_agents_guest_common::prelude::*;
use hyperlight_agents_common::structs::agent_message::AgentMessage;

pub const PROCESS_HTTP_RESPONSE: &str = "ProcessHttpResponse";

pub fn find_title_links<'a>(html: &'a str) -> Vec<(&'a str, &'a str)> {
    let re = Regex::new(r#"<span class="titleline"><a href="([^"]+)">([^<]+)</a>"#).unwrap();
    let mut results = Vec::new();

    for cap in re.captures_iter(html) {
        if let (Some(url), Some(title)) = (cap.get(1), cap.get(2)) {
            results.push((url.as_str(), title.as_str()));
        }
    }

    results
}

fn process_http_response(function_call: &FunctionCall) -> Result<Vec<u8>> {
    if let Some(parameters) = &function_call.parameters {
        if let Some(ParameterValue::String(http_body)) = parameters.get(0) {
            let mut result = String::from("Top Hacker News stories:\n");
            let title_links = find_title_links(&http_body);
            for (i, (url, title)) in title_links.iter().enumerate() {
                result.push_str(&format!("{}. {} - {}\n", i + 1, title, url));
            }
            let message = AgentMessage {
                callback: None,
                message: Some(result),
                guest_message: None,
                is_success: true,
            };
            return send_message_to_host_method(constants::HostMethod::FinalResult.as_ref(), message);
        }
    }
    Err(HyperlightGuestError::new(
        ErrorCode::GuestFunctionParameterTypeMismatch,
        "Invalid parameters passed to process_http_response".to_string(),
    ))
}

fn guest_run(function_call: &FunctionCall) -> Result<Vec<u8>> {
    // For now, just trigger the HTTP fetch
    let _params = function_call.parameters.as_ref();
    let message = AgentMessage {
        callback: Some(PROCESS_HTTP_RESPONSE.to_string()),
        message: Some("https://news.ycombinator.com/".to_string()),
        guest_message: None,
        is_success: true,
    };
    send_message_to_host_method(
        constants::HostMethod::FetchData.as_ref(), message
    )
}

fn get_mcp_tool(_function_call: &FunctionCall) -> Result<Vec<u8>> {
    let tool = Tool {
        name: "Top HN Links".to_string(),
        description: Some("Fetches the top links from Hacker News".to_string()),
        annotations: None,
        input_schema: ToolInputSchema::new(Vec::new(), None),
        output_schema: None,
        title: None,
        meta: None,
    };
    let serialized = serde_json::to_string(&tool).unwrap();

    Ok(get_flatbuffer_result(serialized.as_str()))
}

#[no_mangle]
pub extern "C" fn hyperlight_main() {
    register_guest_function(
        PROCESS_HTTP_RESPONSE,
        &[ParameterType::String],
        ReturnType::String,
        process_http_response as usize,
    );
    register_guest_function(
        constants::GuestMethod::Run.as_ref(),
        &[ParameterType::String],
        ReturnType::String,
        guest_run as usize,
    );
    register_guest_function(
        constants::GuestMethod::GetMCPTool.as_ref(),
        &[],
        ReturnType::String,
        get_mcp_tool as usize,
    );
}

#[no_mangle]
pub fn guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
    hyperlight_agents_guest_common::default_guest_dispatch_function(function_call)
}