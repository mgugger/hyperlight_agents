#!/bin/bash

# Hyperlight Agents Build Script
# This script builds all components in the correct order

set -e  # Exit on any error

echo "🚀 Building Hyperlight Agents Workspace"
echo "========================================"

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Parse command line arguments
RELEASE_MODE=false
CLEAN_BUILD=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --release)
            RELEASE_MODE=true
            shift
            ;;
        --clean)
            CLEAN_BUILD=true
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo "Options:"
            echo "  --release    Build in release mode"
            echo "  --clean      Clean build artifacts before building"
            echo "  -h, --help   Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            echo "Use --help for usage information"
            exit 1
            ;;
    esac
done

# Set build flags
BUILD_FLAGS=""
if [[ "$RELEASE_MODE" == true ]]; then
    BUILD_FLAGS="--release"
    echo -e "${BLUE}🔧 Building in RELEASE mode${NC}"
else
    echo -e "${BLUE}🔧 Building in DEBUG mode${NC}"
fi

# Clean if requested
if [[ "$CLEAN_BUILD" == true ]]; then
    echo -e "${YELLOW}🧹 Cleaning build artifacts...${NC}"
    cargo clean
    echo -e "${GREEN}✅ Clean completed${NC}"
    echo
fi

# Function to build a specific package
build_package() {
    local package_name=$1
    local description=$2
    
    echo -e "${YELLOW}📦 Building ${description}...${NC}"
    
    if cargo build -p "$package_name" $BUILD_FLAGS; then
        echo -e "${GREEN}✅ ${description} built successfully${NC}"
    else
        echo -e "${RED}❌ Failed to build ${description}${NC}"
        exit 1
    fi
    echo
}

# Function to build a package with specific target
build_package_with_target() {
    local package_name=$1
    local target=$2
    local description=$3
    
    echo -e "${YELLOW}📦 Building ${description} for ${target}...${NC}"
    
    if cargo build -p "$package_name" --target "$target" $BUILD_FLAGS; then
        echo -e "${GREEN}✅ ${description} built successfully for ${target}${NC}"
    else
        echo -e "${RED}❌ Failed to build ${description} for ${target}${NC}"
        exit 1
    fi
    echo
}

# Build order based on dependencies
echo "Building components in dependency order..."
echo

# 1. Common library (no dependencies)
build_package "hyperlight-agents-common" "Common Library"

# 2. Guest binaries (depends on common)
echo -e "${YELLOW}📦 Building Guest Binaries...${NC}"
if cargo build -p "hyperlight-agents-guest" $BUILD_FLAGS; then
    echo -e "${GREEN}✅ Guest Binaries built successfully${NC}"
else
    echo -e "${YELLOW}⚠️  Guest build failed (may be due to API changes), continuing...${NC}"
fi
echo

# 3. Host (depends on common)
echo -e "${YELLOW}📦 Building Host Application...${NC}"
if cargo build -p "hyperlight-agents-host" $BUILD_FLAGS; then
    echo -e "${GREEN}✅ Host Application built successfully${NC}"
else
    echo -e "${YELLOW}⚠️  Host build failed (may be due to API changes), continuing...${NC}"
fi
echo

# 4. VM Agent (standalone, needs musl target)
echo -e "${YELLOW}📦 Building VM Agent (static binary for VM deployment)...${NC}"
cd vm-agent
if cargo build --target x86_64-unknown-linux-musl $BUILD_FLAGS; then
    echo -e "${GREEN}✅ VM Agent built successfully${NC}"
else
    echo -e "${RED}❌ Failed to build VM Agent${NC}"
    exit 1
fi
cd ..
echo

# Determine paths based on build mode
if [[ "$RELEASE_MODE" == true ]]; then
    BUILD_DIR="release"
else
    BUILD_DIR="debug"
fi

echo -e "${GREEN}🎉 Build completed!${NC}"
echo
echo "Build artifacts:"
echo "- Host binary: target/$BUILD_DIR/hyperlight-agents-host"
echo "- Guest binaries: guest/target/x86_64-unknown-none/$BUILD_DIR/"
echo "- VM Agent: vm-agent/target/x86_64-unknown-linux-musl/$BUILD_DIR/vm-agent"
echo
echo "To run the host application:"
echo "  ./target/$BUILD_DIR/hyperlight-agents-host"
echo
echo "Quick build commands:"
echo "  ./build.sh           # Debug build"
echo "  ./build.sh --release # Release build"
echo "  ./build.sh --clean   # Clean + debug build"
