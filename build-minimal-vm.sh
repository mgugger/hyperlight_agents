#!/bin/bash

# Build script for minimal VM images with Rust VSOCK agent
# This creates a fast-booting VM image for Firecracker/QEMU

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_IMAGES_DIR="$SCRIPT_DIR/vm-images"
VM_AGENT_DIR="$SCRIPT_DIR/vm-agent"
BUILD_DIR="$VM_IMAGES_DIR/build"

echo "Building minimal VM images with Rust VSOCK agent..."

# Create directories
mkdir -p "$VM_IMAGES_DIR" "$BUILD_DIR"

# Function to check dependencies
check_dependencies() {
    local missing_deps=()
    
    for cmd in cargo rustc; do
        if ! command -v "$cmd" &> /dev/null; then
            missing_deps+=("$cmd")
        fi
    done
    
    if [ ${#missing_deps[@]} -ne 0 ]; then
        echo "ERROR: Missing dependencies: ${missing_deps[*]}"
        echo "Please install Rust: https://rustup.rs/"
        exit 1
    fi
    
    echo "✓ Rust toolchain found"
}

# Function to build the VM agent
build_vm_agent() {
    echo "Building VM agent..."
    
    cd "$VM_AGENT_DIR"
    
    # Build for x86_64 statically linked for maximum compatibility
    cargo build --release --target x86_64-unknown-linux-musl 2>/dev/null || {
        echo "Installing musl target..."
        rustup target add x86_64-unknown-linux-musl
        cargo build --release --target x86_64-unknown-linux-musl
    }
    
    local agent_binary="$VM_AGENT_DIR/target/x86_64-unknown-linux-musl/release/vm-agent"
    
    if [ ! -f "$agent_binary" ]; then
        echo "ERROR: Failed to build vm-agent binary"
        exit 1
    fi
    
    # Copy to build directory
    cp "$agent_binary" "$BUILD_DIR/vm-agent"
    chmod +x "$BUILD_DIR/vm-agent"
    
    echo "✓ VM agent built: $(du -h "$BUILD_DIR/vm-agent" | cut -f1)"
    
    cd "$SCRIPT_DIR"
}

# Function to create minimal rootfs
create_minimal_rootfs() {
    echo "Creating minimal rootfs..."
    
    local rootfs_dir="$BUILD_DIR/rootfs"
    local rootfs_image="$VM_IMAGES_DIR/rootfs-template.ext4"
    
    # Clean and create rootfs directory
    rm -rf "$rootfs_dir"
    mkdir -p "$rootfs_dir"
    
    # Create directory structure
    mkdir -p "$rootfs_dir"/{bin,sbin,etc,proc,sys,dev,tmp,var/log,usr/bin,usr/sbin}
    
    # Install busybox if available
    if command -v busybox &> /dev/null; then
        cp "$(which busybox)" "$rootfs_dir/bin/"
        
        # Create symlinks for common commands
        cd "$rootfs_dir/bin"
        for cmd in sh ash bash dash echo cat ls cp mv rm mkdir rmdir chmod chown \
                   mount umount sleep date grep sed awk cut sort uniq head tail \
                   ps kill killall top htop free df du tar gzip gunzip nc netstat \
                   ping wget curl git make gcc g++ python python3 node npm; do
            ln -sf busybox "$cmd" 2>/dev/null || true
        done
        cd "$SCRIPT_DIR"
        
        echo "✓ Busybox installed with common utilities"
    else
        echo "WARNING: busybox not found - VM will have limited utilities"
        echo "Install busybox-static for better compatibility"
    fi
    
    # Install our VM agent
    cp "$BUILD_DIR/vm-agent" "$rootfs_dir/usr/bin/"
    
    # Create init script that starts our VM agent
    cat > "$rootfs_dir/init" << 'EOF'
#!/bin/sh

# Minimal init script for VM
echo "Starting minimal VM..."

# Mount essential filesystems
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true  
mount -t devtmpfs devtmpfs /dev 2>/dev/null || mknod /dev/null c 1 3

# Set hostname from kernel command line or default
HOSTNAME=$(cat /proc/cmdline | grep -o 'hostname=[^ ]*' | cut -d= -f2)
if [ -n "$HOSTNAME" ]; then
    echo "$HOSTNAME" > /proc/sys/kernel/hostname
    echo "Hostname set to: $HOSTNAME"
fi

# Get VM ID and CID from kernel command line
VM_ID=$(cat /proc/cmdline | grep -o 'vm_id=[^ ]*' | cut -d= -f2)
CID=$(cat /proc/cmdline | grep -o 'cid=[^ ]*' | cut -d= -f2)

# Use defaults if not provided
VM_ID=${VM_ID:-"vm-$(cat /proc/sys/kernel/random/uuid | cut -d- -f1)"}
CID=${CID:-100}

echo "VM ID: $VM_ID, CID: $CID"

# Start the VM agent
echo "Starting VM agent..."
if [ -x /usr/bin/vm-agent ]; then
    /usr/bin/vm-agent --vm-id "$VM_ID" --cid "$CID" &
    AGENT_PID=$!
    echo "VM agent started with PID: $AGENT_PID"
else
    echo "ERROR: VM agent binary not found"
fi

# Keep the system running
while true; do
    if [ -n "$AGENT_PID" ] && ! kill -0 "$AGENT_PID" 2>/dev/null; then
        echo "VM agent died, restarting..."
        /usr/bin/vm-agent --vm-id "$VM_ID" --cid "$CID" &
        AGENT_PID=$!
    fi
    sleep 10
done
EOF
    
    chmod +x "$rootfs_dir/init"
    
    # Create the rootfs image
    local rootfs_size="64M"
    
    echo "Creating rootfs image ($rootfs_size)..."
    dd if=/dev/zero of="$rootfs_image" bs=1M count=64 status=none
    mkfs.ext4 -F "$rootfs_image" >/dev/null
    
    # Mount and copy files
    local mount_point="$BUILD_DIR/mnt"
    mkdir -p "$mount_point"
    
    sudo mount -o loop "$rootfs_image" "$mount_point"
    sudo cp -a "$rootfs_dir"/* "$mount_point/"
    sudo umount "$mount_point"
    
    echo "✓ Rootfs created: $(du -h "$rootfs_image" | cut -f1)"
}

# Function to download/prepare kernel
prepare_kernel() {
    echo "Preparing kernel..."
    
    local kernel_path="$VM_IMAGES_DIR/vmlinux"
    
    # Check if we already have a kernel
    if [ -f "$kernel_path" ]; then
        echo "✓ Kernel already exists: $(du -h "$kernel_path" | cut -f1)"
        return
    fi
    
    # Try to find a suitable kernel
    local potential_kernels=(
        "/boot/vmlinuz-$(uname -r)"
        "/boot/vmlinuz"
        "/usr/src/linux/arch/x86/boot/bzImage"
    )
    
    for kernel_src in "${potential_kernels[@]}"; do
        if [ -f "$kernel_src" ]; then
            echo "Found kernel: $kernel_src"
            cp "$kernel_src" "$kernel_path"
            echo "✓ Kernel prepared: $(du -h "$kernel_path" | cut -f1)"
            return
        fi
    done
    
    echo "WARNING: No suitable kernel found!"
    echo "You can:"
    echo "1. Build a minimal kernel with CONFIG_VIRTIO_VSOCK=y"
    echo "2. Download a pre-built kernel"
    echo "3. Copy your current kernel: cp /boot/vmlinuz-\$(uname -r) $kernel_path"
    
    # Create a placeholder
    touch "$kernel_path"
}

# Function to create Firecracker config template
create_firecracker_template() {
    echo "Creating Firecracker configuration template..."
    
    cat > "$VM_IMAGES_DIR/firecracker-template.json" << 'EOF'
{
  "boot-source": {
    "kernel_image_path": "vm-images/vmlinux",
    "boot_args": "console=ttyS0 reboot=k panic=1 pci=off vm_id=VM_ID_PLACEHOLDER cid=CID_PLACEHOLDER"
  },
  "drives": [{
    "drive_id": "rootfs",
    "path_on_host": "vm-images/rootfs-template.ext4",
    "is_root_device": true,
    "is_read_only": false
  }],
  "machine-config": {
    "vcpu_count": 1,
    "mem_size_mib": 128,
    "ht_enabled": false
  },
  "vsock": {
    "guest_cid": "CID_PLACEHOLDER",
    "uds_path": "/tmp/firecracker-VM_ID_PLACEHOLDER.sock"
  }
}
EOF
    
    echo "✓ Firecracker template created"
}

# Function to create usage instructions
create_usage_instructions() {
    cat > "$VM_IMAGES_DIR/README.md" << 'EOF'
# Minimal VM Images for Hyperlight Agents

This directory contains minimal VM images optimized for fast boot times and low resource usage.

## Files

- `vmlinux` - Minimal Linux kernel with VSOCK support
- `rootfs-template.ext4` - Minimal root filesystem with Rust VSOCK agent
- `firecracker-template.json` - Firecracker configuration template

## Usage with Firecracker

```bash
# Copy template and customize
cp firecracker-template.json my-vm.json
sed -i 's/VM_ID_PLACEHOLDER/my-vm-1/g' my-vm.json
sed -i 's/CID_PLACEHOLDER/101/g' my-vm.json

# Start VM
firecracker --api-sock /tmp/firecracker-my-vm-1.sock --config-file my-vm.json
```

## Usage with QEMU

```bash
qemu-system-x86_64 \
  -enable-kvm \
  -m 128 \
  -kernel vmlinux \
  -drive file=rootfs-template.ext4,format=raw,if=virtio \
  -append "console=ttyS0 root=/dev/vda vm_id=my-vm-1 cid=101" \
  -device vhost-vsock-pci,guest-cid=101 \
  -nographic
```

## Boot Time Comparison

- **Firecracker**: ~125ms
- **QEMU**: ~1-2s  
- **Ubuntu Cloud-init**: ~30-60s

## VM Agent Features

The embedded Rust VM agent:
- Connects to host via VSOCK on port 1234
- Listens for commands on port 1235
- Executes shell commands with timeout support
- Reports results back to host
- Automatic reconnection and error handling
- Statically linked for maximum compatibility

EOF
    
    echo "✓ Usage instructions created"
}

# Main execution
main() {
    echo "=========================================="
    echo "Building Minimal VM Images"
    echo "=========================================="
    
    check_dependencies
    build_vm_agent
    create_minimal_rootfs
    prepare_kernel
    create_firecracker_template
    create_usage_instructions
    
    echo ""
    echo "=========================================="
    echo "Build Complete!"
    echo "=========================================="
    echo ""
    echo "VM images created in: $VM_IMAGES_DIR"
    echo ""
    echo "Image sizes:"
    if [ -f "$VM_IMAGES_DIR/vmlinux" ]; then
        echo "  Kernel:  $(du -h "$VM_IMAGES_DIR/vmlinux" | cut -f1)"
    fi
    if [ -f "$VM_IMAGES_DIR/rootfs-template.ext4" ]; then
        echo "  Rootfs:  $(du -h "$VM_IMAGES_DIR/rootfs-template.ext4" | cut -f1)"
    fi
    echo "  Agent:   $(du -h "$BUILD_DIR/vm-agent" | cut -f1)"
    echo ""
    echo "Expected boot time:"
    echo "  Firecracker: ~125ms"
    echo "  QEMU:        ~1-2s"
    echo ""
    echo "See $VM_IMAGES_DIR/README.md for usage instructions"
}

# Run if script is executed directly
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
