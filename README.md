# Hyperlight Agents

A Rust-based system for running agents in Firecracker VMs with VSOCK communication and MCP server integration.

## Architecture

This workspace contains multiple Rust projects:

- **`hyperlight_agents_common`** - Common library shared between all components
- **`guest/`** - Guest binaries that run inside Hyperlight sandboxes
- **`host/`** - Host application that manages VMs and provides MCP server
- **`vm-agent/`** - Static binary that runs inside Firecracker VMs

## Building

### Quick Start
```bash
# Build all main components (common, guest, host)
cargo build

# Build everything including VM agent
cargo build --workspace
cargo build -p vm-agent --target x86_64-unknown-linux-musl --release

# Or use the aliases:
cargo build-all        # Build all components
cargo build-release    # Build all in release mode
cargo build-vm-agent   # Build just the VM agent
cargo build-main       # Build main components (excluding vm-agent)
```

### Individual Components
```bash
# Build common library
cargo build -p hyperlight-agents-common

# Build guest binaries
cargo build -p hyperlight-agents-guest

# Build host application
cargo build -p hyperlight-agents-host

# Build VM agent (requires musl target)
cargo build -p vm-agent --target x86_64-unknown-linux-musl --release
```

### Running
```bash
# Run the host application
cargo run -p hyperlight-agents-host

# Or use the alias:
cargo run-host
```

## Dependencies

The build order is automatically handled by Cargo based on dependencies:
1. `hyperlight_agents_common` (no dependencies)
2. `guest` (depends on common)
3. `host` (depends on common)
4. `vm-agent` (standalone, builds with musl target)

## Development

```bash
# Check all code
cargo check --workspace

# Run tests
cargo test --workspace

# Clean all build artifacts
cargo clean --workspace
# Or: cargo clean-all

# Format code
cargo fmt --all

# Lint code
cargo clippy --workspace
```

## Targets

- Main components build for the default system target
- VM agent builds for `x86_64-unknown-linux-musl` to create static binaries for VM deployment

## Build Artifacts

- Host binary: `target/debug/hyperlight-agents-host` (or `target/release/`)
- Guest binaries: `guest/target/x86_64-unknown-none/debug/`
- VM Agent: `vm-agent/target/x86_64-unknown-linux-musl/release/vm-agent`

---

## Legacy Documentation

The demo implements hyperlight agents that can:
1. Fetch the top stories from Hacker News via HTTP requests
2. Process the responses asynchronously using callbacks
3. Demonstrate secure host function calls from sandboxed environments
4. **NEW: Create and manage lightweight VMs for isolated build/test environments**

Each agent runs in its own isolated sandbox with controlled access to system resources. The architecture supports running multiple agents in parallel, each with its own communication channel and state.

## Features

### Core Agent System
- Sandboxed execution using Hyperlight
- Async HTTP requests with callbacks
- MCP (Model Context Protocol) server integration
- Multiple agents running in parallel

### VM Management (NEW)
- Create lightweight VMs using QEMU/KVM
- VSOCK communication between host and VMs
- VM agents that can execute build/test commands
- Cloud-init based VM provisioning
- Automatic VM agent deployment

## Available Agents

1. **TopHNLinks**: Fetches top Hacker News stories
2. **VmBuilder**: Creates and manages VMs for isolated development environments

## Run

```bash
cd guest && cargo build
cd host && cargo run
```

Access the tools via MCP on localhost:3000/sse.

## VM Management Setup

### Prerequisites

1. **QEMU and KVM support**:
   ```bash
   sudo apt-get install qemu-system-x86_64 qemu-utils
   ```

2. **Ubuntu Cloud Image** (recommended):
   ```bash
   wget https://cloud-images.ubuntu.com/releases/22.04/release/ubuntu-22.04-server-cloudimg-amd64.img
   sudo mkdir -p /var/lib/cloud/images/
   sudo mv ubuntu-22.04-server-cloudimg-amd64.img /var/lib/cloud/images/
   ```

3. **ISO creation tools**:
   ```bash
   sudo apt-get install genisoimage  # or xorriso
   ```

### VM Agent Features

The VM agent automatically:
- Connects to the host via VSOCK
- Executes commands received from hyperlight agents
- Reports results back to the host
- Supports timeouts and working directory changes
- Includes development tools (git, python, nodejs, build tools)

### Example VM Operations

Using the VmBuilder agent through MCP:

1. **Create a VM**:
   ```json
   {
     "action": "create_vm",
     "vm_id": "build-vm-1"
   }
   ```

2. **Execute commands in VM**:
   ```json
   {
     "action": "execute_command",
     "vm_id": "build-vm-1", 
     "command": "git clone https://github.com/user/repo.git",
     "working_dir": "/workspace"
   }
   ```

3. **Build and test**:
   ```json
   {
     "action": "execute_command",
     "vm_id": "build-vm-1",
     "command": "cargo build && cargo test",
     "working_dir": "/workspace/repo",
     "timeout_seconds": 300
   }
   ```

4. **Destroy VM**:
   ```json
   {
     "action": "destroy_vm",
     "vm_id": "build-vm-1"
   }
   ```

## Architecture

```
┌─────────────────┐    ┌─────────────────┐    ┌─────────────────┐
│   MCP Client    │    │  Hyperlight     │    │   Lightweight   │
│   (VS Code)     │◄──►│    Host         │◄──►│      VMs        │
│                 │    │                 │    │                 │
└─────────────────┘    │  ┌───────────┐  │    │  ┌───────────┐  │
                       │  │  Agent 1  │  │    │  │ VM Agent  │  │
                       │  │           │  │    │  │           │  │
                       │  └───────────┘  │    │  └───────────┘  │
                       │  ┌───────────┐  │    │                 │
                       │  │  Agent 2  │  │    │  ┌───────────┐  │
                       │  │ (VmBuilder)│  │    │  │ VM Agent  │  │
                       │  └───────────┘  │    │  │           │  │
                       └─────────────────┘    │  └───────────┘  │
                                              └─────────────────┘
                                                     │
                                                  VSOCK
                                              Communication
```

## Security

- Hyperlight agents run in isolated sandboxes
- VMs provide additional isolation for build/test operations
- VSOCK communication is secure and isolated
- Each VM has its own ephemeral filesystem
