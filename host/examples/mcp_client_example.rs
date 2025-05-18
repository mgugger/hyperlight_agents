use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::io::{self, BufRead};
use std::time::Duration;

#[derive(serde::Deserialize, Debug, Clone)]
struct AgentInfo {
    id: String,
    name: String,
    description: String,
}

fn main() {
    let client = Client::new();
    let mcp_url = "http://127.0.0.1:3000";

    println!("MCP Client Example");
    println!("=================");
    println!("Commands:");
    println!("  list - List all available agents");
    println!("  broadcast - Send a message to multiple agents");
    println!("  <agent_id>:<message> - Send a message to an agent");
    println!("  exit - Quit the application");
    println!();
    println!("Example: agent1:Hello, world!");

    let stdin = io::stdin();
    let mut handle = stdin.lock();
    let mut line = String::new();

    loop {
        print!("> ");
        io::Write::flush(&mut io::stdout()).unwrap();
        line.clear();

        if handle.read_line(&mut line).unwrap() == 0 {
            break;
        }

        let input = line.trim();
        if input == "exit" {
            break;
        }

        // Handle special commands
        if input == "list" {
            list_agents(&client, mcp_url);
            continue;
        } else if input == "broadcast" {
            broadcast_mode(&client, mcp_url);
            continue;
        }

        let parts: Vec<&str> = input.splitn(3, ':').collect();
        if parts.len() < 2 {
            println!("Invalid format. Use: agent_id:function:message");
            continue;
        }

        let agent_id = parts[0];
        let message = if parts.len() >= 2 { parts[1] } else { "" };

        send_message_to_agent(&client, mcp_url, agent_id, message);
    }
}

fn send_message_to_agent(client: &Client, mcp_url: &str, agent_id: &str, message: &str) {
    // Construct the MCP request
    let mcp_request = json!({
        "recipient": agent_id,
        "message": message,
        "function": "Run"
    });

    // Send the request
    println!("Sending request to agent '{}'...", agent_id);
    match client
        .post(mcp_url)
        .json(&mcp_request)
        .timeout(Duration::from_secs(30))
        .send()
    {
        Ok(response) => match response.json::<Value>() {
            Ok(json_response) => {
                println!(
                    "Response: {}",
                    serde_json::to_string_pretty(&json_response).unwrap()
                );
            }
            Err(e) => {
                println!("Failed to parse response: {}", e);
            }
        },
        Err(e) => {
            println!("Request failed: {}", e);
        }
    }
}

fn fetch_agents(client: &Client, mcp_url: &str) -> Vec<AgentInfo> {
    match client
        .get(&format!("{}/agents", mcp_url))
        .timeout(Duration::from_secs(5))
        .send()
    {
        Ok(response) => match response.json::<Vec<AgentInfo>>() {
            Ok(agents) => agents,
            Err(e) => {
                println!("Failed to parse agents list: {}", e);
                Vec::new()
            }
        },
        Err(e) => {
            println!("Failed to fetch agents: {}", e);
            Vec::new()
        }
    }
}

fn list_agents(client: &Client, mcp_url: &str) {
    println!("Fetching available agents...");

    let agents = fetch_agents(client, mcp_url);

    if agents.is_empty() {
        println!("No agents available.");
        return;
    }

    println!("\nAvailable Agents:");
    println!("=================");

    for agent in agents {
        println!("ID: {}", agent.id);
        println!("Name: {}", agent.name);
        println!("Description: {}", agent.description);
        println!("-------------------------------------");
    }
}

fn broadcast_mode(client: &Client, mcp_url: &str) {
    println!("Fetching available agents...");

    let agents = fetch_agents(client, mcp_url);

    if agents.is_empty() {
        println!("No agents available.");
        return;
    }

    println!("\nSelect agents to broadcast to (comma-separated numbers):");
    for (i, agent) in agents.iter().enumerate() {
        println!("[{}] {} - {}", i + 1, agent.name, agent.id);
    }
    println!("[0] Cancel");
    println!("[a] All agents");

    print!("Enter agent numbers: ");
    io::Write::flush(&mut io::stdout()).unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let selection = input.trim().to_lowercase();

    if selection.is_empty() || selection == "0" {
        println!("Selection canceled");
        return;
    }

    // Determine selected agents
    let selected_agents: Vec<&AgentInfo> = if selection == "a" {
        agents.iter().collect()
    } else {
        selection
            .split(',')
            .filter_map(|s| s.trim().parse::<usize>().ok())
            .filter(|&idx| idx > 0 && idx <= agents.len())
            .map(|idx| &agents[idx - 1])
            .collect()
    };

    if selected_agents.is_empty() {
        println!("No valid agents selected");
        return;
    }

    println!("\nSelected {} agents:", selected_agents.len());
    for agent in &selected_agents {
        println!("- {} ({})", agent.name, agent.id);
    }

    println!("\nSelect function:");
    println!("[1] process - Process a message");
    println!("[2] query - Query for information");
    println!("[3] custom - Enter custom function name");

    print!("Enter function number (or 0 to cancel): ");
    io::Write::flush(&mut io::stdout()).unwrap();

    input.clear();
    io::stdin().read_line(&mut input).unwrap();
    let function_index = input.trim().parse::<usize>().unwrap_or(0);

    if function_index == 0 {
        println!("Selection canceled");
        return;
    }

    println!(
        "\nEnter message to broadcast to {} agents:",
        selected_agents.len()
    );
    print!("> ");
    io::Write::flush(&mut io::stdout()).unwrap();

    input.clear();
    io::stdin().read_line(&mut input).unwrap();
    let message = input.trim();

    if message.is_empty() {
        println!("Empty message, canceling");
        return;
    }

    println!(
        "\nBroadcasting message to {} agents...",
        selected_agents.len()
    );
    for agent in selected_agents {
        println!("\nSending to {} ({}):", agent.name, agent.id);
        send_message_to_agent(client, mcp_url, &agent.id, message);
    }
}
