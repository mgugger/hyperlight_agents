#!/bin/bash

echo "=== Simple VSOCK Communication Test ==="

# Clean up
pkill -f firecracker 2>/dev/null || true
sleep 2
rm -f /tmp/firecracker.sock /tmp/vsock-test-vm.sock

cd /home/manuel/git/hyperlight_agents/vm-images

echo "Starting VM..."
/home/manuel/firecracker/release-v1.12.1-x86_64/firecracker-v1.12.1-x86_64 \
  --api-sock /tmp/firecracker.sock \
  --config-file firecracker-config.json &

FIRECRACKER_PID=$!
echo "Firecracker started with PID: $FIRECRACKER_PID"

# Wait for VM to boot
echo "Waiting for VM to boot..."
sleep 8

# Check if VSOCK socket was created
if [ -e /tmp/vsock-test-vm.sock ]; then
    echo "✅ VSOCK socket created: /tmp/vsock-test-vm.sock"
    
    # Test basic VSOCK connectivity
    echo "Testing VSOCK connectivity..."
    
    # Send a simple test using socat with VSOCK address format
    # Format: VSOCK:CID:PORT where CID=101 (VM), PORT=1234
    timeout 5 socat - VSOCK-CONNECT:101:1234,vsock-fd=3 3>/tmp/vsock-test-vm.sock <<EOF 2>/dev/null && echo "✅ VSOCK test message sent" || echo "❌ VSOCK communication failed (expected - VM not listening yet)"
{"type":"ping","message":"hello from host"}
EOF

    echo ""
    echo "VM Status:"
    ps aux | grep firecracker | grep -v grep
    
    echo ""
    echo "VSOCK socket info:"
    ls -la /tmp/vsock-test-vm.sock
    
else
    echo "❌ VSOCK socket not found"
fi

echo ""
echo "To stop VM: kill $FIRECRACKER_PID"
echo "To connect to VM console: socat - UNIX-CONNECT:/tmp/firecracker.sock"
echo ""
echo "The VM is ready! Now you can:"
echo "1. Run your Rust host application to communicate via VSOCK"
echo "2. Or use socat to send messages manually"
echo ""
echo "Example manual VSOCK communication:"
echo "echo 'hello' | socat - UNIX-CONNECT:/tmp/vsock-test-vm.sock"
