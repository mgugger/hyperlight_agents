#!/bin/bash

echo "=== Adding Rust VM Agent to existing rootfs ==="

# First, build the Rust agent (configured for static linking in Cargo.toml)
echo "Building Rust vm-agent..."
cd /home/manuel/git/hyperlight_agents/vm-agent
cargo build --release

# Mount the existing rootfs
mkdir -p /tmp/vm-mount
sudo mount -o loop /home/manuel/git/hyperlight_agents/vm-images/rootfs.ext4 /tmp/vm-mount

echo "Current rootfs contents:"
sudo ls -la /tmp/vm-mount/

# Copy the static Rust agent binary
echo "Installing static Rust vm-agent binary..."
sudo cp /home/manuel/git/hyperlight_agents/vm-agent/target/x86_64-unknown-linux-musl/release/vm-agent /tmp/vm-mount/vm-agent
sudo chmod +x /tmp/vm-mount/vm-agent

# Remove old shell script if it exists
sudo rm -f /tmp/vm-mount/vm-agent.sh

# Create or update the init script to call our agent
sudo tee /tmp/vm-mount/init > /dev/null << 'EOF'
#!/bin/sh
echo "Starting VM with Rust agent..."

# Mount essential filesystems
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

# Set up PATH
export PATH=/bin:/sbin:/usr/bin:/usr/sbin

# Start the Rust VM agent in background
echo "Launching Rust VM agent..."
/vm-agent &

# Start BusyBox shell for console access  
echo "Starting BusyBox shell..."
exec /bin/sh
EOF

sudo chmod +x /tmp/vm-mount/init

# Also create it as /sbin/init to handle different init paths
sudo mkdir -p /tmp/vm-mount/sbin
sudo cp /tmp/vm-mount/init /tmp/vm-mount/sbin/init 2>/dev/null || true
sudo chmod +x /tmp/vm-mount/sbin/init 2>/dev/null || true

echo "Updated init script:"
sudo cat /tmp/vm-mount/init

echo "Rust VM Agent binary installed:"
sudo ls -la /tmp/vm-mount/vm-agent

# Unmount
sudo umount /tmp/vm-mount
rmdir /tmp/vm-mount

echo "✅ Rust VM Agent added to rootfs successfully!"
echo "Now test your VM with:"
echo "cd /home/manuel/git/hyperlight_agents/vm-images"
echo "/home/manuel/firecracker/release-v1.12.1-x86_64/firecracker-v1.12.1-x86_64 --api-sock /tmp/firecracker.sock --config-file firecracker-config.json"
