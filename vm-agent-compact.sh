#!/bin/sh
# VM Agent - VSOCK communication

echo "Starting VM Agent..."

# Parse kernel parameters
VM_ID="unknown"
CID="101"
if [ -f /proc/cmdline ]; then
    for param in $(cat /proc/cmdline); do
        case $param in
            VM_ID=*) VM_ID="${param#VM_ID=}" ;;
            CID=*) CID="${param#CID=}" ;;
        esac
    done
fi

echo "VM Agent: VM_ID=$VM_ID, CID=$CID"

# Mount filesystems
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

# Load VSOCK modules
modprobe vsock 2>/dev/null || true
modprobe vmw_vsock_virtio_transport 2>/dev/null || true

# Execute command function
execute_command() {
    local cmd_json="$1"
    local cmd_id=$(echo "$cmd_json" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
    local command=$(echo "$cmd_json" | sed -n 's/.*"command":"\([^"]*\)".*/\1/p')
    
    if [ -z "$command" ]; then
        echo "Error: No command in JSON"
        return 1
    fi
    
    echo "Executing: $command (ID: $cmd_id)"
    
    local temp_out=$(mktemp)
    local temp_err=$(mktemp)
    
    eval "$command" >"$temp_out" 2>"$temp_err"
    local exit_code=$?
    
    local stdout_content=$(cat "$temp_out" | tr '\n' ' ')
    local stderr_content=$(cat "$temp_err" | tr '\n' ' ')
    
    rm -f "$temp_out" "$temp_err"
    
    local result="{\"type\":\"command_result\",\"id\":\"$cmd_id\",\"vm_id\":\"$VM_ID\",\"exit_code\":$exit_code,\"stdout\":\"$stdout_content\",\"stderr\":\"$stderr_content\"}"
    
    echo "Result: $result"
    echo "$result" >> /tmp/results.log
}

# Handle messages
handle_message() {
    local message="$1"
    local msg_type=$(echo "$message" | sed -n 's/.*"type":"\([^"]*\)".*/\1/p')
    
    case "$msg_type" in
        "execute_command")
            execute_command "$message"
            ;;
        "ping")
            echo "{\"type\":\"pong\",\"vm_id\":\"$VM_ID\",\"cid\":$CID}" >> /tmp/results.log
            ;;
    esac
}

# Create communication interface
mkfifo /tmp/cmd_pipe 2>/dev/null || true

echo "VSOCK Agent ready - CID:$CID"
echo "Test: echo '{\"type\":\"execute_command\",\"id\":\"test1\",\"command\":\"ls\"}' > /tmp/cmd.json"

# Background listener
while true; do
    if [ -f /tmp/cmd.json ]; then
        handle_message "$(cat /tmp/cmd.json)"
        rm -f /tmp/cmd.json
    fi
    sleep 1
done &

echo "Agent started"
exec /bin/sh
