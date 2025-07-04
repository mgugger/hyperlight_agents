# Hyperlight Agents with MCP

A Rust-based system for running code with Hyperlight and being able to run commands against sandboxed firecracker VMs. Use MCP to call the tools / hyperlight functions / agents.

## Quick Start

Run the complete build and setup process:

```bash
cargo run -p xtask -- run
```

## Available Commands

| Command | Description |
|---------|-------------|
| `run` | Complete build and setup process (default) |
| `build-guest` | Build guest package only |
| `build-vm-agent` | Build vm-agent binary only |
| `build-base-rootfs` | Create base rootfs image (without agent) |
| `download-kernel` | Download kernel binary if missing |
| `download-firecracker` | Download firecracker binary if missing |
| `run-host` | Run host package |
| `clean` | Clean all downloaded and built artifacts |

### Examples

```bash
# Build individual components
cargo run -p xtask -- build-guest
cargo run -p xtask -- build-vm-agent

# Download dependencies
cargo run -p xtask -- download-kernel
cargo run -p xtask -- download-firecracker

# Run host only (after building)
cargo run -p xtask -- run-host

# Clean everything
cargo run -p xtask -- clean
```

## What `run` does

1. Builds the guest package
2. Creates rootfs with vm-agent
3. Downloads kernel (if missing)
4. Downloads firecracker (if missing)
5. Runs the host application
