#!/bin/bash

# VM Agent Script - To be deployed inside VMs
# This script connects to the host via VSOCK and executes commands

VM_ID="${VM_ID:-$(hostname)}"
HOST_CID="2"  # Host CID is always 2
REGISTER_PORT="1234"
COMMAND_PORT="1235"
SCRIPT_DIR="/usr/local/bin"
LOG_FILE="/var/log/vm-agent.log"

# Logging function
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') - $1" | tee -a "$LOG_FILE"
}

# Install dependencies if needed
install_dependencies() {
    if ! command -v socat &> /dev/null; then
        log "Installing socat..."
        if command -v apt-get &> /dev/null; then
            sudo apt-get update && sudo apt-get install -y socat
        elif command -v yum &> /dev/null; then
            sudo yum install -y socat
        elif command -v dnf &> /dev/null; then
            sudo dnf install -y socat
        else
            log "ERROR: Cannot install socat - no supported package manager found"
            exit 1
        fi
    fi

    if ! command -v jq &> /dev/null; then
        log "Installing jq..."
        if command -v apt-get &> /dev/null; then
            sudo apt-get install -y jq
        elif command -v yum &> /dev/null; then
            sudo yum install -y jq
        elif command -v dnf &> /dev/null; then
            sudo dnf install -y jq
        else
            log "WARNING: jq not available - JSON parsing will be limited"
        fi
    fi
}

# Register with host
register_with_host() {
    local cid="$1"
    local registration_msg="{\"type\":\"register\",\"vm_id\":\"$VM_ID\",\"cid\":$cid}"
    
    log "Registering with host: $registration_msg"
    echo "$registration_msg" | socat - VSOCK-CONNECT:$HOST_CID:$REGISTER_PORT
    
    if [ $? -eq 0 ]; then
        log "Successfully registered with host"
        return 0
    else
        log "Failed to register with host"
        return 1
    fi
}

# Execute command and send result back
execute_command() {
    local command_json="$1"
    
    # Parse JSON (basic parsing without jq if not available)
    if command -v jq &> /dev/null; then
        local cmd=$(echo "$command_json" | jq -r '.command // empty')
        local args_array=$(echo "$command_json" | jq -r '.args[]? // empty')
        local working_dir=$(echo "$command_json" | jq -r '.working_dir // empty')
        local timeout_seconds=$(echo "$command_json" | jq -r '.timeout_seconds // empty')
        local command_id=$(echo "$command_json" | jq -r '.id // empty')
    else
        # Basic JSON parsing without jq
        local cmd=$(echo "$command_json" | sed -n 's/.*"command":"\([^"]*\)".*/\1/p')
        local working_dir=$(echo "$command_json" | sed -n 's/.*"working_dir":"\([^"]*\)".*/\1/p')
        local timeout_seconds=$(echo "$command_json" | sed -n 's/.*"timeout_seconds":\([0-9]*\).*/\1/p')
        local command_id=$(echo "$command_json" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    fi
    
    if [ -z "$cmd" ]; then
        log "ERROR: No command specified in JSON: $command_json"
        return 1
    fi
    
    log "Executing command: $cmd"
    
    # Change working directory if specified
    if [ -n "$working_dir" ] && [ -d "$working_dir" ]; then
        cd "$working_dir"
        log "Changed working directory to: $working_dir"
    fi
    
    # Set timeout if specified
    local timeout_cmd=""
    if [ -n "$timeout_seconds" ] && [ "$timeout_seconds" -gt 0 ]; then
        timeout_cmd="timeout ${timeout_seconds}s"
        log "Using timeout: ${timeout_seconds}s"
    fi
    
    # Execute the command
    local start_time=$(date +%s)
    local temp_stdout=$(mktemp)
    local temp_stderr=$(mktemp)
    
    if [ -n "$timeout_cmd" ]; then
        $timeout_cmd bash -c "$cmd" > "$temp_stdout" 2> "$temp_stderr"
    else
        bash -c "$cmd" > "$temp_stdout" 2> "$temp_stderr"
    fi
    
    local exit_code=$?
    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    
    local stdout_content=$(cat "$temp_stdout")
    local stderr_content=$(cat "$temp_stderr")
    
    # Clean up temp files
    rm -f "$temp_stdout" "$temp_stderr"
    
    # Escape JSON special characters
    stdout_content=$(echo "$stdout_content" | sed 's/\\/\\\\/g; s/"/\\"/g; s/$/\\n/' | tr -d '\n' | sed 's/\\n$//')
    stderr_content=$(echo "$stderr_content" | sed 's/\\/\\\\/g; s/"/\\"/g; s/$/\\n/' | tr -d '\n' | sed 's/\\n$//')
    
    # Send result back to host
    local result_msg="{\"type\":\"command_result\",\"id\":\"$command_id\",\"vm_id\":\"$VM_ID\",\"exit_code\":$exit_code,\"stdout\":\"$stdout_content\",\"stderr\":\"$stderr_content\",\"duration\":$duration}"
    
    log "Command completed with exit code $exit_code in ${duration}s"
    log "Sending result back to host"
    
    echo "$result_msg" | socat - VSOCK-CONNECT:$HOST_CID:$REGISTER_PORT
    
    if [ $? -eq 0 ]; then
        log "Successfully sent command result to host"
    else
        log "Failed to send command result to host"
    fi
}

# Listen for commands from host
listen_for_commands() {
    log "Starting command listener on port $COMMAND_PORT"
    
    while true; do
        # Listen for incoming commands
        local command_json=$(socat - VSOCK-LISTEN:$COMMAND_PORT,reuseaddr,fork)
        
        if [ -n "$command_json" ]; then
            log "Received command: $command_json"
            execute_command "$command_json" &
        fi
        
        sleep 1
    done
}

# Main function
main() {
    log "Starting VM Agent for VM: $VM_ID"
    
    # Install dependencies
    install_dependencies
    
    # Get VM's CID (this is a placeholder - in real VSOCK, the CID would be assigned)
    local vm_cid=$(cat /proc/sys/kernel/random/uuid | tr -d '-' | head -c 8)
    vm_cid=$((0x$vm_cid % 1000 + 100))  # Convert to number between 100-1099
    
    log "VM CID: $vm_cid"
    
    # Register with host in a loop
    while true; do
        if register_with_host "$vm_cid"; then
            break
        else
            log "Registration failed, retrying in 5 seconds..."
            sleep 5
        fi
    done
    
    # Start command listener in background
    listen_for_commands &
    local listener_pid=$!
    
    # Keep the main process alive and periodically re-register
    while true; do
        sleep 30
        if ! kill -0 $listener_pid 2>/dev/null; then
            log "Command listener died, restarting..."
            listen_for_commands &
            listener_pid=$!
        fi
        
        # Re-register periodically to keep connection alive
        register_with_host "$vm_cid" >/dev/null 2>&1
    done
}

# Handle signals
trap 'log "VM Agent shutting down"; exit 0' SIGTERM SIGINT

# Run as daemon if requested
if [ "$1" = "--daemon" ]; then
    # Create daemon
    if [ -f "/etc/systemd/system/vm-agent.service" ]; then
        log "Starting as systemd service"
        systemctl start vm-agent
    else
        log "Starting as background daemon"
        nohup "$0" > "$LOG_FILE" 2>&1 &
        echo $! > /var/run/vm-agent.pid
    fi
else
    main
fi
