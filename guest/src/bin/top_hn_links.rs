#![no_std]
#![no_main]

extern crate alloc;
extern crate hyperlight_guest;

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use hyperlight_agents_common::traits::agent::{Param, ParamType};
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

use hyperlight_agents_common::constants;
use hyperlight_agents_common::Agent;

use strum_macros::AsRefStr;

pub struct TopHNLinksAgent;

#[derive(Debug, PartialEq, AsRefStr)]
enum AgentConstants {
    ProcessHttpResponse,
}

impl Agent for TopHNLinksAgent {
    type Error = HyperlightGuestError;

    fn get_name(&self) -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        Ok(get_flatbuffer_result("TopHNLinks"))
    }

    fn get_description(&self) -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        Ok(get_flatbuffer_result(
            "An Agent that returns the current Top Hacker News Links",
        ))
    }

    fn get_params(&self) -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        let mut params: Vec<Param> = Vec::new();
        let param = Param {
            name: "test".as_bytes().to_owned(),
            description: Some("test".as_bytes().to_owned()),
            param_type: ParamType::String,
            required: true,
        };
        params.push(param);

        let mut serialized = String::new();
        serialized.push_str("[");
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                serialized.push_str(", ");
            }
            serialized.push_str(&format!(
                "{{\"name\": \"{}\", \"required\": {}}}",
                core::str::from_utf8(&p.name).unwrap_or("invalid"),
                p.required
            ));
        }
        serialized.push_str("]");

        Ok(get_flatbuffer_result(serialized.as_str()))
    }

    fn process(
        &self,
        _function_call: &FunctionCall,
    ) -> core::result::Result<Vec<u8>, HyperlightGuestError> {
        send_message_to_host_method(
            constants::HostMethod::FetchData.as_ref(),
            "https://news.ycombinator.com/",
            "",
            AgentConstants::ProcessHttpResponse.as_ref(),
        )
    }
}

fn send_message_to_host_method(
    method_name: &str,
    guest_message: &str,
    message: &str,
    callback_function: &str,
) -> Result<Vec<u8>> {
    let message = format!("{}{}", guest_message, message);
    call_host_function(
        method_name,
        Some(Vec::from(&[
            ParameterValue::String(message.to_string()),
            ParameterValue::String(callback_function.to_string()),
        ])),
        ReturnType::String,
    )?;

    Ok(get_flatbuffer_result(
        format!("Host function {} called successfully", method_name).as_str(),
    ))
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
    if let ParameterValue::String(http_body) = &function_call.parameters.as_ref().unwrap()[0] {
        let mut result = String::from("Top Hacker News stories:\n");
        //result.push_str(http_body);
        let title_links = find_title_links(&http_body);
        for (i, (url, title)) in title_links.iter().enumerate() {
            result.push_str(&format!("{}. {} - {}\n", i + 1, title, url));
        }
        send_message_to_host_method(
            constants::HostMethod::FinalResult.as_ref(),
            result.as_str(),
            "",
            "",
        )
    } else {
        Err(HyperlightGuestError::new(
            ErrorCode::GuestFunctionParameterTypeMismatch,
            "Invalid parameters passed to guest_function".to_string(),
        ))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn hyperlight_main() {
    let top_hn_links_def = GuestFunctionDefinition::new(
        constants::GuestMethod::Run.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        TopHNLinksAgent::process as usize,
    );
    register_function(top_hn_links_def);

    let process_http_response_def = GuestFunctionDefinition::new(
        AgentConstants::ProcessHttpResponse.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        process_http_response as usize,
    );
    register_function(process_http_response_def);

    let get_name_def = GuestFunctionDefinition::new(
        constants::GuestMethod::GetName.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        TopHNLinksAgent::get_name as usize,
    );
    register_function(get_name_def);

    let get_description_def = GuestFunctionDefinition::new(
        constants::GuestMethod::GetDescription.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        TopHNLinksAgent::get_description as usize,
    );
    register_function(get_description_def);

    let get_params_def = GuestFunctionDefinition::new(
        constants::GuestMethod::GetParams.as_ref().to_string(),
        Vec::from(&[ParameterType::String]),
        ReturnType::String,
        TopHNLinksAgent::get_params as usize,
    );
    register_function(get_params_def);
}

#[unsafe(no_mangle)]
pub fn guest_dispatch_function(function_call: FunctionCall) -> Result<Vec<u8>> {
    Err(HyperlightGuestError::new(
        ErrorCode::GuestFunctionNotFound,
        function_call.function_name.clone(),
    ))
}
