#!/bin/sh
# Simple init script for hyperlight VM
echo "Starting Hyperlight VM Agent..."

# Mount essential filesystems
mount -t proc proc /proc
mount -t sysfs sysfs /sys
#mount -t devtmpfs devtmpfs /dev

# Configure loopback interface
ip link set lo up
ip addr add 127.0.0.1/8 dev lo

# Load VSOCK modules
modprobe vsock 2>/dev/null || true
modprobe vmw_vsock_virtio_transport 2>/dev/null || true

# Configure HTTP proxy environment variables
export http_proxy=http://127.0.0.1:8080
export https_proxy=http://127.0.0.1:8080
export no_proxy=127.0.0.1,localhost
export HTTP_PROXY=http://127.0.0.1:8080
export HTTPS_PROXY=http://127.0.0.1:8080
export NO_PROXY=127.0.0.1,localhost
export RUST_LOG=debug

echo "Starting VM Agent on VSOCK port 1234..."
echo "HTTP proxy configured at http://127.0.0.1:8080"
echo -1000 > /proc/self/oom_score_adj
exec nice -n -10 /usr/bin/vm-agent
