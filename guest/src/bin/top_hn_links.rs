#![no_std]
#![no_main]

extern crate alloc;
extern crate hyperlight_guest;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::{format, vec};
use hyperlight_agents_common::{
    constants, Annotations, Role, Tool, ToolAnnotations, ToolInputSchema,
};
use hyperlight_common::flatbuffer_wrappers::function_call::FunctionCall;
use hyperlight_common::flatbuffer_wrappers::function_types::{
    ParameterType, ParameterValue, ReturnType,
};
use hyperlight_common::flatbuffer_wrappers::guest_error::ErrorCode;
use hyperlight_common::flatbuffer_wrappers::util::get_flatbuffer_result;
use hyperlight_guest::error::{HyperlightGuestError, Result};
use hyperlight_guest_bin::guest_function::definition::GuestFunctionDefinition;
use hyperlight_guest_bin::guest_function::register::register_function;
use hyperlight_guest_bin::host_comm::call_host_function;
use regex::Regex;
use strum_macros::AsRefStr;

#[derive(Debug, PartialEq, AsRefStr)]
enum AgentConstants {
    ProcessHttpResponse,
}

fn send_message_to_host_method(
    method_name: &str,
    guest_message: &str,
    message: &str,
    callback_function: &str,
) -> Result<Vec<u8>> {
    let message = format!("{}{}", guest_message, message);

    let _res = call_host_function::<String>(
        method_name,
        Some(Vec::from(&[
            ParameterValue::String(message.to_string()),
            ParameterValue::String(callback_function.to_string()),
        ])),
        ReturnType::String,
    )?;

    Ok(get_flatbuffer_result("Success"))
}

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
            return send_message_to_host_method(
                constants::HostMethod::FinalResult.as_ref(),
                &result,
                "",
                "",
            );
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
    send_message_to_host_method(
        constants::HostMethod::FetchData.as_ref(),
        "https://news.ycombinator.com/",
        "",
        AgentConstants::ProcessHttpResponse.as_ref(),
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
    // Register the main run function
    register_function(GuestFunctionDefinition::new(
        constants::GuestMethod::Run.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        guest_run as usize,
    ));

    // Register metadata functions - these should not take parameters
    register_function(GuestFunctionDefinition::new(
        constants::GuestMethod::GetMCPTool.as_ref().to_string(),
        Vec::new(),
        ReturnType::String,
        get_mcp_tool as usize,
    ));

    // Register callback function
    register_function(GuestFunctionDefinition::new(
        AgentConstants::ProcessHttpResponse.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_http_response as usize,
    ));
}

#[no_mangle]
pub fn guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
    Err(HyperlightGuestError::new(
        ErrorCode::GuestFunctionNotFound,
        function_call.function_name.clone(),
    ))
}
