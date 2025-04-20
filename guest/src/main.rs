#![no_std]
#![no_main]

extern crate alloc;
extern crate hyperlight_guest;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use hyperlight_common::flatbuffer_wrappers::function_call::FunctionCall;
use hyperlight_common::flatbuffer_wrappers::function_types::{
    ParameterType, ParameterValue, ReturnType,
};
use hyperlight_common::flatbuffer_wrappers::guest_error::ErrorCode;
use hyperlight_common::flatbuffer_wrappers::util::get_flatbuffer_result;
use hyperlight_guest::error::{HyperlightGuestError, Result};
use hyperlight_guest::guest_function_definition::GuestFunctionDefinition;
use hyperlight_guest::guest_function_register::register_function;
use hyperlight_guest::host_function_call::call_host_function;

use regex::Regex;

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

fn send_message_to_host_method(
    method_name: &str,
    guest_message: &str,
    message: &str,
) -> Result<Vec<u8>> {
    let message = format!("{}{}", guest_message, message);
    call_host_function(
        method_name,
        Some(Vec::from(&[
            ParameterValue::String(message.to_string()),
            ParameterValue::String("ProcessHttpResponse".to_string()),
        ])),
        ReturnType::String,
    )?;

    Ok(get_flatbuffer_result(
        format!("Host function {} called successfully", method_name).as_str(),
    ))
}

fn process_http_response(function_call: &FunctionCall) -> Result<Vec<u8>> {
    if let ParameterValue::String(http_body) = &function_call.parameters.as_ref().unwrap()[0] {
        let mut result = String::from("Top Hacker News stories:\n");
        //result.push_str(http_body);
        let title_links = find_title_links(&http_body);
        for (i, (url, title)) in title_links.iter().enumerate() {
            result.push_str(&format!("{}. {} - {}\n", i + 1, title, url));
        }
        send_message_to_host_method("FinalAnswerHostMethod", result.as_str(), "")
    } else {
        Err(HyperlightGuestError::new(
            ErrorCode::GuestFunctionParameterTypeMismatch,
            "Invalid parameters passed to guest_function".to_string(),
        ))
    }
}

fn top_hn_links() -> Result<Vec<u8>> {
    send_message_to_host_method("HttpGet", "https://news.ycombinator.com/", "")
}

#[unsafe(no_mangle)]
pub extern "C" fn hyperlight_main() {
    let top_hn_links_def = GuestFunctionDefinition::new(
        "TopHNLinks".to_string(),
        Vec::from(&[]),
        ReturnType::String,
        top_hn_links as usize,
    );
    register_function(top_hn_links_def);

    let process_http_response_def = GuestFunctionDefinition::new(
        "ProcessHttpResponse".to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_http_response as usize,
    );
    register_function(process_http_response_def);
}

#[unsafe(no_mangle)]
pub fn guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
    Err(HyperlightGuestError::new(
        ErrorCode::GuestFunctionNotFound,
        function_call.function_name.clone(),
    ))
}
