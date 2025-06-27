#!/bin/bash

echo "=== Testing VSOCK Communication with VM ==="

# Start VM in background
echo "Starting VM..."
cd /home/manuel/git/hyperlight_agents/vm-images

# Clean up any existing sockets and processes
echo "Cleaning up previous instances..."
pkill -f firecracker 2>/dev/null || true
sleep 2
rm -f /tmp/firecracker.sock /tmp/vsock-test-vm.sock /tmp/vsock_host.sock /tmp/vsock.sock

# Wait a moment for cleanup
sleep 1

# Start firecracker in background
/home/manuel/firecracker/release-v1.12.1-x86_64/firecracker-v1.12.1-x86_64 \
  --api-sock /tmp/firecracker.sock \
  --config-file firecracker-config.json &

FIRECRACKER_PID=$!
echo "Firecracker started with PID: $FIRECRACKER_PID"

# Wait for VM to boot
echo "Waiting for VM to boot..."
sleep 10

# Check if VSOCK socket was created
echo "Checking VSOCK socket..."
if [ -e /tmp/vsock-test-vm.sock ]; then
    echo "✅ VSOCK socket created: /tmp/vsock-test-vm.sock"
else
    echo "❌ VSOCK socket not found"
fi

# Try to listen for VM registration
echo "Listening for VM registration messages..."
if command -v socat >/dev/null 2>&1; then
    echo "Using socat to listen on VSOCK..."
    
    # The VSOCK socket created by Firecracker allows bidirectional communication
    # Let's try to send a message to the VM via VSOCK
    echo "Sending test message to VM via VSOCK..."
    if [ -e /tmp/vsock-test-vm.sock ]; then
        echo '{"type":"test","message":"hello from host"}' | socat - UNIX-CONNECT:/tmp/vsock-test-vm.sock 2>/dev/null && echo "✅ Message sent to VM" || echo "❌ Failed to send message to VM"
    else
        echo "❌ VSOCK socket not available for communication"
    fi
    
    sleep 5
else
    echo "socat not available for advanced testing"
fi

# Show VM process status
echo "VM Process Status:"
ps aux | grep firecracker | grep -v grep

echo ""
echo "To stop the VM:"
echo "kill $FIRECRACKER_PID"
echo "rm -f /tmp/firecracker.sock /tmp/vsock-test-vm.sock"

echo ""
echo "To connect to VM console (if available):"
echo "socat - /tmp/firecracker.sock"
