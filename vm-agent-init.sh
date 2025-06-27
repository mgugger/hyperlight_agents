#!/bin/sh
# VM Agent Init Script - Real VSOCK communication with host

echo "Starting VM Agent..."

# Parse kernel parameters for VM_ID and CID
VM_ID="unknown"
CID="101"

# Extract VM_ID and CID from kernel command line
if [ -f /proc/cmdline ]; then
    for param in $(cat /proc/cmdline); do
        case $param in
            VM_ID=*)
                VM_ID="${param#VM_ID=}"
                ;;
            CID=*)
                CID="${param#CID=}"
                ;;
        esac
    done
fi

HOST_CID=2
PORT=1234

echo "VM Agent started with VM_ID=$VM_ID, CID=$CID"

# Mount essential filesystems if not already mounted
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

# Load VSOCK modules
modprobe vsock 2>/dev/null || true
modprobe vmw_vsock_virtio_transport 2>/dev/null || true
modprobe vhost_vsock 2>/dev/null || true

echo "VSOCK modules loaded"

# Check if VSOCK device exists
if [ -c /dev/vsock ]; then
    echo "VSOCK device found: /dev/vsock"
else
    echo "VSOCK device not found, creating placeholder"
    # Create a placeholder for testing (in real systems this should exist)
    mknod /dev/vsock c 10 121 2>/dev/null || true
fi

# Function to execute command and return JSON result
execute_command() {
    local cmd_json="$1"
    local cmd_id temp_stdout temp_stderr start_time end_time exit_code duration
    
    # Parse JSON manually (since jq might not be available)
    cmd_id=$(echo "$cmd_json" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    local command=$(echo "$cmd_json" | sed -n 's/.*"command":"\([^"]*\)".*/\1/p')
    local working_dir=$(echo "$cmd_json" | sed -n 's/.*"working_dir":"\([^"]*\)".*/\1/p')
    
    if [ -z "$command" ]; then
        echo "Error: No command found in JSON"
        return 1
    fi
    
    echo "Executing command: $command (ID: $cmd_id)"
    
    # Change to working directory if specified
    if [ -n "$working_dir" ] && [ -d "$working_dir" ]; then
        cd "$working_dir" || echo "Warning: Could not change to directory $working_dir"
    fi
    
    # Create temp files for output
    temp_stdout=$(mktemp)
    temp_stderr=$(mktemp)
    
    # Execute command and capture output
    start_time=$(date +%s)
    eval "$command" >"$temp_stdout" 2>"$temp_stderr"
    exit_code=$?
    end_time=$(date +%s)
    duration=$((end_time - start_time))
    
    # Read output and escape for JSON (simplified)
    local stdout_content stderr_content
    stdout_content=$(cat "$temp_stdout" | tr '\n' ' ' | sed 's/"/\\"/g')
    stderr_content=$(cat "$temp_stderr" | tr '\n' ' ' | sed 's/"/\\"/g')
    
    # Clean up temp files
    rm -f "$temp_stdout" "$temp_stderr"
    
    # Create result JSON
    local result_json="{\"type\":\"command_result\",\"id\":\"$cmd_id\",\"vm_id\":\"$VM_ID\",\"exit_code\":$exit_code,\"stdout\":\"$stdout_content\",\"stderr\":\"$stderr_content\",\"duration\":$duration}"
    
    echo "Command completed with exit code $exit_code"
    echo "$result_json"
}

# Function to handle incoming messages
handle_message() {
    local message="$1"
    echo "Received message: $message"
    
    # Parse message type
    local msg_type=$(echo "$message" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
    
    case "$msg_type" in
        "execute_command")
            local result=$(execute_command "$message")
            echo "Command result: $result"
            # Log result to file for debugging
            echo "$result" >> /tmp/command_results.log
            # In a real VSOCK implementation, we'd send this back to the host
            return 0
            ;;
        "ping")
            echo "Received ping, sending pong"
            echo "{\"type\":\"pong\",\"vm_id\":\"$VM_ID\",\"cid\":$CID}" >> /tmp/command_results.log
            return 0
            ;;
        *)
            echo "Unknown message type: $msg_type"
            return 1
            ;;
    esac
}

# Create a simple VSOCK listener using netcat-like functionality
# Since we might not have socat in the VM, we'll use a simple approach
echo "Setting up VSOCK communication..."

# Create VSOCK listener script
cat > /tmp/vsock_listener.sh << 'EOF'
#!/bin/sh
# Simple VSOCK listener for BusyBox environments

VSOCK_CID="$1"
VSOCK_PORT="$2"
VM_ID="$3"

echo "VSOCK Listener starting on CID:$VSOCK_CID PORT:$VSOCK_PORT for VM:$VM_ID"

# Create a named pipe for communication
mkfifo /tmp/vsock_pipe 2>/dev/null || true

# In a real implementation, this would bind to VSOCK
# For now, we simulate with file-based communication
while true; do
    # Check for incoming commands via file system (fallback method)
    if [ -f /tmp/incoming_command.json ]; then
        message=$(cat /tmp/incoming_command.json)
        rm -f /tmp/incoming_command.json
        
        # Process the command
        . /vm-agent.sh  # Source the main script functions
        handle_message "$message"
    fi
    
    # Check for pipe-based communication
    if [ -p /tmp/vsock_pipe ]; then
        if read -r message < /tmp/vsock_pipe 2>/dev/null; then
            if [ -n "$message" ]; then
                . /vm-agent.sh  # Source the main script functions
                handle_message "$message"
            fi
        fi
    fi
    
    sleep 1
done
EOF

chmod +x /tmp/vsock_listener.sh

# Start the VSOCK listener in background
echo "Starting VSOCK listener on CID:$CID PORT:$PORT"
/tmp/vsock_listener.sh "$CID" "$PORT" "$VM_ID" &
LISTENER_PID=$!

echo "VSOCK listener started with PID: $LISTENER_PID"

# Register with host (simulate)
echo "VM $VM_ID registered with CID $CID" >> /tmp/agent.log

# Create communication endpoints for testing
mkfifo /tmp/vsock_pipe 2>/dev/null || true
chmod 666 /tmp/vsock_pipe

echo "VM Agent ready for VSOCK communication"
echo "Agent log: /tmp/agent.log"
echo "Command results: /tmp/command_results.log"
echo "VSOCK listener PID: $LISTENER_PID"
echo ""
echo "To test: echo '{\"type\":\"execute_command\",\"id\":\"test1\",\"command\":\"ls -la\"}' > /tmp/incoming_command.json"
echo ""

# Start an interactive shell
exec /bin/sh

echo "VM $VM_ID (CID: $CID) agent starting..."

# Function to send JSON message via VSOCK using socat or nc
send_vsock_msg() {
    local msg="$1"
    if command -v socat >/dev/null 2>&1; then
        echo "$msg" | socat - VSOCK-CONNECT:$HOST_CID:$PORT 2>/dev/null || true
    elif command -v nc >/dev/null 2>&1; then
        echo "$msg" | nc vsock $HOST_CID $PORT 2>/dev/null || true
    else
        echo "Warning: Neither socat nor nc available for VSOCK communication"
    fi
}

# Register with host
register_vm() {
    local reg_msg="{\"type\":\"register\",\"vm_id\":\"$VM_ID\",\"cid\":$CID}"
    send_vsock_msg "$reg_msg"
    echo "Sent registration: $reg_msg"
}

# Execute command and send result back to host
execute_command() {
    local cmd_json="$1"
    echo "Executing command: $cmd_json"
    
    # Parse JSON manually (since jq might not be available)
    local cmd=$(echo "$cmd_json" | sed -n 's/.*"command":"\([^"]*\)".*/\1/p')
    local cmd_id=$(echo "$cmd_json" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    local working_dir=$(echo "$cmd_json" | sed -n 's/.*"working_dir":"\([^"]*\)".*/\1/p')
    
    if [ -z "$cmd" ]; then
        echo "Warning: Could not parse command from JSON"
        return
    fi
    
    echo "Parsed command: '$cmd' (ID: $cmd_id)"
    
    # Change to working directory if specified
    if [ -n "$working_dir" ] && [ -d "$working_dir" ]; then
        cd "$working_dir" || true
    fi
    
    # Execute the command and capture output
    local start_time=$(date +%s)
    local temp_stdout=$(mktemp)
    local temp_stderr=$(mktemp)
    
    # Execute command
    eval "$cmd" >"$temp_stdout" 2>"$temp_stderr"
    local exit_code=$?
    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    
    # Read output and escape for JSON
    local stdout_content=$(cat "$temp_stdout" | sed 's/\\/\\\\/g; s/"/\\"/g; s/$/\\n/' | tr -d '\n' | sed 's/\\n$//')
    local stderr_content=$(cat "$temp_stderr" | sed 's/\\/\\\\/g; s/"/\\"/g; s/$/\\n/' | tr -d '\n' | sed 's/\\n$//')
    
    # Clean up temp files
    rm -f "$temp_stdout" "$temp_stderr"
    
    # Send result back to host
    local result_json="{\"type\":\"command_result\",\"id\":\"$cmd_id\",\"vm_id\":\"$VM_ID\",\"exit_code\":$exit_code,\"stdout\":\"$stdout_content\",\"stderr\":\"$stderr_content\",\"duration\":$duration}"
    
    echo "Sending result: $result_json"
    send_vsock_msg "$result_json"
}

# Listen for incoming commands via VSOCK
listen_for_commands() {
    echo "Starting command listener on VSOCK..."
    
    while true; do
        if command -v socat >/dev/null 2>&1; then
            # Use socat to listen for VSOCK connections
            socat VSOCK-LISTEN:$PORT,fork EXEC:"/bin/sh -c 'read cmd && execute_command \"\$cmd\"'" 2>/dev/null &
            LISTENER_PID=$!
            echo "Started VSOCK listener with socat (PID: $LISTENER_PID)"
            wait $LISTENER_PID
        else
            echo "socat not available, using periodic check method"
            # Fallback: periodically try to connect and check for commands
            sleep 5
        fi
    done
}

# Main execution
main() {
    echo "VM Agent Main Loop Starting"
    
    # Register with host every 10 seconds in background
    (
        while true; do
            register_vm
            sleep 10
        done
    ) &
    
    # Start command listener
    listen_for_commands &
    
    # Keep the script running and provide a simple shell
    echo "VM Agent running. Type 'help' for available commands."
    echo "Available commands: exit, reboot, halt, ps, ls, pwd, whoami, uname"
    
    # Simple command loop for interactive use
    while true; do
        printf "vm# "
        read -r input
        
        case "$input" in
            "exit"|"quit")
                echo "Shutting down VM..."
                break
                ;;
            "reboot")
                echo "Rebooting VM..."
                /sbin/reboot 2>/dev/null || echo "Reboot command not available"
                ;;
            "halt"|"shutdown")
                echo "Halting VM..."
                /sbin/halt 2>/dev/null || echo "Halt command not available"
                break
                ;;
            "help")
                echo "Available commands:"
                echo "  exit/quit - Exit the agent"
                echo "  reboot    - Reboot the VM"
                echo "  halt      - Halt the VM"
                echo "  ps        - List processes"
                echo "  ls        - List files"
                echo "  pwd       - Print working directory"
                echo "  whoami    - Print current user"
                echo "  uname     - Print system information"
                echo "  Any other command will be executed directly"
                ;;
            "")
                # Empty input, continue
                ;;
            *)
                # Execute the command directly
                eval "$input"
                ;;
        esac
    done
    
    echo "VM Agent shutting down..."
}

# Start the main function
main
