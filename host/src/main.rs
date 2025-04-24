use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hyperlight_common::flatbuffer_wrappers::function_types::{ParameterValue, ReturnType};
use hyperlight_host::func::{HostFunction2, ReturnValue};
use hyperlight_host::sandbox::SandboxConfiguration;
use hyperlight_host::sandbox_state::sandbox::EvolvableSandbox;
use hyperlight_host::sandbox_state::transition::Noop;
use hyperlight_host::{MultiUseSandbox, UninitializedSandbox};

mod host_functions;
use host_functions::network_functions::http_request;
use hyperlight_agents_common::constants;
use reqwest::blocking::Client;
use std::sync::mpsc::{Receiver, Sender, channel};

struct Agent {
    id: String,
    name: String,
    description: String,
    sandbox: MultiUseSandbox,
    tx: Sender<(Option<String>, String)>,
    rx: Receiver<(Option<String>, String)>, // (response, callback_name)
}

fn main() -> hyperlight_host::Result<()> {
    let http_client = Arc::new(
        reqwest::blocking::ClientBuilder::new()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap(),
    );

    let agent_ids: Vec<String> = std::fs::read_dir("./../guest/target/x86_64-unknown-none/debug/")
        .expect("Failed to read directory")
        .filter_map(|entry| {
            entry.ok().and_then(|e| {
                let path = e.path();
                if path.is_file()
                    && !path.to_string_lossy().ends_with(".d")
                    && !path.to_string_lossy().ends_with(".cargo-lock")
                {
                    Some(path.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
        })
        .collect();
    let mut agents = Vec::new();

    for agent_id in agent_ids {
        let agent = create_agent(
            agent_id.to_string(),
            http_client.clone(),
            agent_id.to_string(),
        )?;
        agents.push(agent);
    }

    // senders
    let mut tx_senders = Vec::new();
    for agent in &agents {
        tx_senders.push((agent.id.clone(), agent.tx.clone()));
    }

    // Start agent tasks in separate threads
    let mut handles = Vec::new();
    for mut agent in agents {
        let handle = thread::spawn(move || {
            run_agent_event_loop(&mut agent);
        });
        handles.push(handle);
    }

    if let Some((id, tx)) = tx_senders.first() {
        tx.send((None, "TopHNLinks".to_string()))
            .expect(&format!("Failed to send message to agent {}", id));
    }

    // Wait for all agents to complete
    for handle in handles {
        let _ = handle.join();
    }

    Ok(())
}

fn create_agent(
    agent_id: String,
    http_client: Arc<Client>,
    binary_path: String,
) -> hyperlight_host::Result<Agent> {
    // Create a channel for communication
    let (tx, rx) = channel::<(Option<String>, String)>();

    // Create sandbox configuration
    let mut sandbox_config = SandboxConfiguration::default();
    sandbox_config.set_input_data_size(100 * 1024 * 1024);
    sandbox_config.set_output_data_size(100 * 1024 * 1024);
    sandbox_config.set_heap_size(100 * 1024 * 1024);

    // Create a sandbox for this agent
    let guest_instance = hyperlight_host::GuestBinary::FilePath(binary_path);

    let mut uninitialized_sandbox =
        UninitializedSandbox::new(guest_instance, Some(sandbox_config), None, None)?;

    // Register host functions specific to this agent
    register_host_functions(
        &mut uninitialized_sandbox,
        tx.clone(),
        http_client,
        &agent_id,
    )?;

    // Initialize the sandbox
    let mut sandbox = uninitialized_sandbox.evolve(Noop::default())?;

    let name = match sandbox
        .call_guest_function_by_name(
            constants::GuestMethod::GetTitle.as_ref(),
            ReturnType::String,
            Some(vec![ParameterValue::String("".to_string())]),
        )
        .unwrap()
    {
        ReturnValue::String(s) => s,
        _ => panic!("Expected a string return value"),
    };

    let description = match sandbox
        .call_guest_function_by_name(
            constants::GuestMethod::GetDescription.as_ref(),
            ReturnType::String,
            Some(vec![ParameterValue::String("".to_string())]),
        )
        .unwrap()
    {
        ReturnValue::String(s) => s,
        _ => panic!("Expected a string return value"),
    };

    Ok(Agent {
        id: agent_id,
        name,
        description,
        sandbox,
        tx,
        rx,
    })
}

fn register_host_functions(
    sandbox: &mut UninitializedSandbox,
    tx: Sender<(Option<String>, String)>,
    http_client: Arc<Client>,
    agent_id: &str,
) -> hyperlight_host::Result<()> {
    // HTTP GET function
    let tx_clone = tx.clone();
    let http_get_fn = Arc::new(Mutex::new(move |url: String, callback_name: String| {
        let client = http_client.clone();
        let sender = tx_clone.clone();

        thread::spawn(move || {
            let response = match http_request(client, &url, "GET", None, None) {
                Ok(resp) => resp,
                Err(e) => format!("HTTP request failed: {}", e),
            };

            if let Err(e) = sender.send((Some(response), callback_name)) {
                eprintln!("Failed to send response: {:?}", e);
            }
        });

        Ok("Http Request sent".to_string())
    }));

    let all_syscalls: Vec<i64> = (0..=500).collect();
    http_get_fn.register_with_extra_allowed_syscalls(
        sandbox,
        constants::HostMethod::FetchData.as_ref(),
        all_syscalls,
    )?;

    // Final answer function
    let agent_id_clone = agent_id.to_string();
    let print_final_answer_fn = Arc::new(Mutex::new(move |answer: String, _param: String| {
        println!("Agent {}: Final answer: {}", agent_id_clone, answer);
        Ok(())
    }));
    print_final_answer_fn.register(sandbox, constants::HostMethod::FinalAnswer.as_ref())?;

    Ok(())
}

fn run_agent_event_loop(agent: &mut Agent) {
    loop {
        match agent.rx.try_recv() {
            Ok((content, callback_name)) => {
                let callback_result = match content {
                    Some(content) => agent.sandbox.call_guest_function_by_name(
                        &callback_name,
                        ReturnType::String,
                        Some(vec![ParameterValue::String(content)]),
                    ),
                    None => agent.sandbox.call_guest_function_by_name(
                        &callback_name,
                        ReturnType::String,
                        Some(vec![]),
                    ),
                };

                match callback_result {
                    Ok(result) => {
                        println!("Agent {} callback returned: {:?}", agent.id, result);
                    }
                    Err(e) => {
                        eprintln!("Agent {} callback error: {:?}", agent.id, e);
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // No responses yet
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                println!("Agent {} channel disconnected", agent.id);
                break;
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}
