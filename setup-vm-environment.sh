#!/bin/bash

# Setup script for Hyperlight VM Agents
# This script downloads and sets up the Ubuntu cloud image and required tools

set -e

echo "Setting up Hyperlight VM Agent environment..."

# Check if running as root for some operations
if [[ $EUID -eq 0 ]]; then
    echo "Note: Running as root. Some operations will be performed system-wide."
    SUDO=""
else
    echo "Note: Running as user. Will use sudo for system operations."
    SUDO="sudo"
fi

# Install required packages
echo "Installing required packages..."
if command -v apt-get &> /dev/null; then
    $SUDO apt-get update
    $SUDO apt-get install -y qemu-system-x86_64 qemu-utils genisoimage wget curl
elif command -v yum &> /dev/null; then
    $SUDO yum install -y qemu-kvm qemu-img xorriso wget curl
elif command -v dnf &> /dev/null; then
    $SUDO dnf install -y qemu-kvm qemu-img xorriso wget curl
else
    echo "ERROR: No supported package manager found (apt-get, yum, or dnf)"
    exit 1
fi

# Create image directory
IMAGE_DIR="/var/lib/cloud/images"
$SUDO mkdir -p "$IMAGE_DIR"

# Download Ubuntu cloud image if not present
UBUNTU_IMAGE="$IMAGE_DIR/ubuntu-22.04-server-cloudimg-amd64.img"
if [ ! -f "$UBUNTU_IMAGE" ]; then
    echo "Downloading Ubuntu 22.04 cloud image..."
    TEMP_IMAGE="/tmp/ubuntu-22.04-server-cloudimg-amd64.img"
    
    wget -O "$TEMP_IMAGE" \
        "https://cloud-images.ubuntu.com/releases/22.04/release/ubuntu-22.04-server-cloudimg-amd64.img"
    
    $SUDO mv "$TEMP_IMAGE" "$UBUNTU_IMAGE"
    $SUDO chmod 644 "$UBUNTU_IMAGE"
    
    echo "Ubuntu cloud image downloaded to $UBUNTU_IMAGE"
else
    echo "Ubuntu cloud image already exists at $UBUNTU_IMAGE"
fi

# Check KVM support
if [ -r /dev/kvm ]; then
    echo "KVM support detected and accessible"
else
    echo "WARNING: KVM not accessible. VMs will run without hardware acceleration."
    echo "You may need to:"
    echo "  1. Enable virtualization in BIOS"
    echo "  2. Load KVM modules: sudo modprobe kvm kvm_intel (or kvm_amd)"
    echo "  3. Add user to kvm group: sudo usermod -a -G kvm \$USER"
fi

# Check VSOCK support
if [ -r /dev/vhost-vsock ]; then
    echo "VSOCK support detected"
else
    echo "WARNING: VSOCK device not found. Loading vhost_vsock module..."
    $SUDO modprobe vhost_vsock || echo "Failed to load vhost_vsock module"
fi

# Create workspace directory
WORKSPACE_DIR="$HOME/hyperlight-vm-workspace"
mkdir -p "$WORKSPACE_DIR"
echo "Created workspace directory: $WORKSPACE_DIR"

# Set up permissions for current user
if [ -n "$SUDO" ]; then
    $SUDO usermod -a -G kvm "$USER" 2>/dev/null || echo "Note: Could not add user to kvm group"
fi

echo ""
echo "Setup complete!"
echo ""
echo "Next steps:"
echo "1. Build the hyperlight agents:"
echo "   cd guest && cargo build"
echo "   cd host && cargo run"
echo ""
echo "2. Access the MCP server at: http://localhost:3000/sse"
echo ""
echo "3. Use the VmBuilder agent to create and manage VMs"
echo ""
echo "If you added yourself to the kvm group, you may need to log out and back in."
echo ""
echo "VM workspace directory: $WORKSPACE_DIR"
echo "Ubuntu cloud image: $UBUNTU_IMAGE"
