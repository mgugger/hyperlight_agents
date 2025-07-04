#!/bin/sh
# Simple init script for hyperlight VM
echo "Starting Hyperlight VM Agent..."

# Mount essential filesystems
mount -t proc proc /proc
mount -t sysfs sysfs /sys
#mount -t devtmpfs devtmpfs /dev

# Load VSOCK modules
modprobe vsock 2>/dev/null || true
modprobe vmw_vsock_virtio_transport 2>/dev/null || true

echo "Starting VM Agent on VSOCK port 1234..."
exec /usr/bin/vm-agent
