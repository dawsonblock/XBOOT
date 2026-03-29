#!/bin/bash
# XBOOT Docker-Only Setup Script
# One-command setup for running XBOOT entirely in Docker
# 
# Usage: ./scripts/setup-docker.sh [command]
# Commands:
#   setup    - Full setup (build, templates, secrets, run)
#   build    - Build Rust binary and Docker image
#   templates - Build guest templates
#   secrets   - Generate API keys and secrets
#   run      - Start with docker-compose
#   test     - Run smoke tests
#   clean    - Remove all containers and volumes

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$PROJECT_ROOT/deploy/docker/docker-compose.yml"
ENV_FILE="$PROJECT_ROOT/deploy/docker/.env"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() {
    echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

# Check Docker is available
check_docker() {
    log_info "Checking Docker..."
    
    if ! command -v docker >/dev/null 2>&1; then
        log_error "Docker not found. Please install Docker first."
        exit 1
    fi
    
    if ! docker info >/dev/null 2>&1; then
        log_error "Docker daemon not running or not accessible."
        exit 1
    fi
    
    log_info "Docker OK"
}

# Check KVM is available on host
check_kvm() {
    log_info "Checking KVM availability..."
    
    if [[ ! -e /dev/kvm ]]; then
        log_error "/dev/kvm not found. KVM is required for XBOOT."
        log_error "Ensure your CPU supports virtualization and KVM is enabled."
        exit 1
    fi
    
    if [[ ! -r /dev/kvm ]] || [[ ! -w /dev/kvm ]]; then
        log_warn "/dev/kvm permissions may be insufficient."
        log_warn "You may need to: sudo usermod -aG kvm $USER"
    fi
    
    log_info "KVM OK"
}

# Build the Rust binary (for mounting into container)
build_binary() {
    log_info "Building zeroboot binary..."
    
    cd "$PROJECT_ROOT"
    
    if [[ ! -f target/release/zeroboot ]]; then
        log_info "Running cargo build --release (this may take a while)..."
        cargo build --release
    else
        log_info "Binary already exists at target/release/zeroboot"
    fi
    
    log_info "Binary OK"
}

# Fetch official artifacts
fetch_artifacts() {
    log_info "Fetching official artifacts..."
    
    cd "$PROJECT_ROOT"
    
    if [[ ! -d /var/lib/zeroboot/artifacts ]]; then
        log_info "Creating artifacts directory..."
        sudo mkdir -p /var/lib/zeroboot/artifacts
    fi
    
    if [[ ! -f /var/lib/zeroboot/artifacts/kernel/vmlinux-5.10.223 ]]; then
        log_info "Downloading Firecracker 1.12.0, kernel, and rootfs..."
        bash scripts/fetch_official_artifacts.sh /var/lib/zeroboot/artifacts
    else
        log_info "Artifacts already present"
    fi
    
    log_info "Artifacts OK"
}

# Build guest templates
build_templates() {
    log_info "Building guest templates..."
    
    cd "$PROJECT_ROOT"
    
    # Check if templates already exist
    if [[ -d work/python ]] && [[ -f work/python/template.manifest.json ]]; then
        log_info "Python template already exists"
    else
        log_info "Building Python template..."
        
        # Prepare base rootfs if needed
        if [[ ! -d /tmp/zb-rootfs-base ]]; then
            log_info "Extracting base rootfs..."
            bash scripts/prepare_rootfs_template.sh \
                /var/lib/zeroboot/artifacts/rootfs/ubuntu-22.04.ext4 \
                /tmp/zb-rootfs-base
        fi
        
        # Build Python
        cp -a /tmp/zb-rootfs-base /tmp/zb-rootfs-python
        make guest-python PY_ROOTFS_TEMPLATE=/tmp/zb-rootfs-python
        make image-python
        make template-python
    fi
    
    if [[ -d work/node ]] && [[ -f work/node/template.manifest.json ]]; then
        log_info "Node.js template already exists"
    else
        log_info "Building Node.js template..."
        
        if [[ ! -d /tmp/zb-rootfs-base ]]; then
            log_info "Extracting base rootfs..."
            bash scripts/prepare_rootfs_template.sh \
                /var/lib/zeroboot/artifacts/rootfs/ubuntu-22.04.ext4 \
                /tmp/zb-rootfs-base
        fi
        
        cp -a /tmp/zb-rootfs-base /tmp/zb-rootfs-node
        bash scripts/install_node_runtime.sh /tmp/zb-rootfs-node /var/lib/zeroboot/artifacts
        make guest-node NODE_ROOTFS_TEMPLATE=/tmp/zb-rootfs-node
        make image-node
        make template-node
    fi
    
    log_info "Templates OK"
}

# Create secrets
setup_secrets() {
    log_info "Setting up secrets..."
    
    mkdir -p "$PROJECT_ROOT/deploy/docker/secrets"
    
    if [[ ! -f "$PROJECT_ROOT/deploy/docker/secrets/api_keys.json" ]]; then
        log_info "Generating API keys..."
        python3 "$PROJECT_ROOT/scripts/make_api_keys.py" \
            --count 1 \
            --out "$PROJECT_ROOT/deploy/docker/secrets/api_keys.json"
    else
        log_info "API keys already exist"
    fi
    
    if [[ ! -f "$PROJECT_ROOT/deploy/docker/secrets/pepper" ]]; then
        log_info "Generating pepper secret..."
        openssl rand -hex 32 > "$PROJECT_ROOT/deploy/docker/secrets/pepper"
    else
        log_info "Pepper already exists"
    fi
    
    log_info "Secrets OK"
}

# Create .env file
setup_env() {
    log_info "Setting up environment..."
    
    if [[ ! -f "$ENV_FILE" ]]; then
        log_info "Creating .env from example..."
        cp "$PROJECT_ROOT/deploy/docker/.env.example" "$ENV_FILE"
        log_info "Environment file created at $ENV_FILE"
        log_info "Review and customize settings if needed"
    else
        log_info "Environment file already exists"
    fi
}

# Build Docker image
build_image() {
    log_info "Building Docker image..."
    
    cd "$PROJECT_ROOT"
    
    docker build \
        -f deploy/docker/Dockerfile.runtime \
        -t xboot-runtime:latest \
        .
    
    log_info "Docker image OK"
}

# Start services
start_services() {
    log_info "Starting XBOOT services..."
    
    cd "$PROJECT_ROOT"
    
    # Create state directory
    mkdir -p deploy/docker/state
    
    docker compose -f "$COMPOSE_FILE" --env-file "$ENV_FILE" up -d
    
    log_info "Services started"
    log_info "Waiting for health checks (10 seconds)..."
    sleep 10
    
    # Check if healthy
    if docker compose -f "$COMPOSE_FILE" ps | grep -q "healthy"; then
        log_info "XBOOT is healthy!"
    else
        log_warn "XBOOT may still be starting. Check logs with:"
        log_warn "  docker compose -f $COMPOSE_FILE logs -f"
    fi
}

# Run smoke tests
run_tests() {
    log_info "Running smoke tests..."
    
    cd "$PROJECT_ROOT"
    
    # Extract API key from secrets
    API_KEY=$(python3 -c "
import json
with open('deploy/docker/secrets/api_keys.json') as f:
    data = json.load(f)
    if 'keys' in data and len(data['keys']) > 0:
        key = data['keys'][0]
        if 'secret' in key:
            print(f\"{key['id']}:{key['secret']}\")
        else:
            print('test-key')
    else:
        print('test-key')
" 2>/dev/null || echo "test-key")
    
    log_info "Using API key: ${API_KEY:0:20}..."
    
    # Wait a bit more if needed
    sleep 5
    
    # Run smoke test
    if ./scripts/smoke_exec.sh "$API_KEY" http://127.0.0.1:8080; then
        log_info "All smoke tests passed!"
    else
        log_error "Smoke tests failed. Check logs:"
        log_error "  docker compose -f $COMPOSE_FILE logs"
        exit 1
    fi
}

# Stop services
stop_services() {
    log_info "Stopping XBOOT services..."
    
    cd "$PROJECT_ROOT"
    
    docker compose -f "$COMPOSE_FILE" down
    
    log_info "Services stopped"
}

# Clean everything
clean_all() {
    log_info "Cleaning up..."
    
    cd "$PROJECT_ROOT"
    
    # Stop and remove containers
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    
    # Remove image
    docker rmi xboot-runtime:latest 2>/dev/null || true
    
    # Clean state
    rm -rf deploy/docker/state
    
    log_info "Cleanup complete"
}

# Show help
show_help() {
    cat << EOF
XBOOT Docker-Only Setup Script

Usage: $0 [command]

Commands:
  setup    - Full setup: build, templates, secrets, run, test
  build    - Build Rust binary and Docker image only
  templates - Build guest templates only
  secrets   - Generate API keys and secrets only
  run      - Start services with docker-compose
  test     - Run smoke tests against running container
  stop     - Stop all services
  clean    - Remove all containers, volumes, and images
  status   - Check service status
  logs     - Show service logs

Examples:
  # Complete one-time setup
  $0 setup

  # Just build everything
  $0 build

  # Start services (assumes already built)
  $0 run

  # Run tests
  $0 test

  # Clean up
  $0 clean

Requirements:
  - Docker with daemon running
  - KVM support on host (/dev/kvm accessible)
  - Rust toolchain (for initial build)
  - Python 3 (for API key generation)
EOF
}

# Check service status
show_status() {
    cd "$PROJECT_ROOT"
    
    echo "=== XBOOT Docker Status ==="
    docker compose -f "$COMPOSE_FILE" ps
    
    echo ""
    echo "=== Health Check ==="
    if curl -fsS http://127.0.0.1:8080/live 2>/dev/null; then
        echo "LIVE: OK"
    else
        echo "LIVE: NOT RESPONDING"
    fi
    
    if curl -fsS http://127.0.0.1:8080/ready 2>/dev/null; then
        echo "READY: OK"
    else
        echo "READY: NOT RESPONDING"
    fi
}

# Show logs
show_logs() {
    cd "$PROJECT_ROOT"
    docker compose -f "$COMPOSE_FILE" logs -f
}

# Main command handler
main() {
    case "${1:-help}" in
        setup)
            check_docker
            check_kvm
            build_binary
            fetch_artifacts
            build_templates
            setup_secrets
            setup_env
            build_image
            start_services
            sleep 15
            run_tests
            log_info "Setup complete! XBOOT is running on http://127.0.0.1:8080"
            ;;
        build)
            check_docker
            build_binary
            fetch_artifacts
            build_templates
            build_image
            log_info "Build complete!"
            ;;
        templates)
            check_docker
            fetch_artifacts
            build_templates
            ;;
        secrets)
            setup_secrets
            setup_env
            ;;
        run)
            check_docker
            check_kvm
            start_services
            log_info "XBOOT started! Access at http://127.0.0.1:8080"
            log_info "Run tests with: $0 test"
            ;;
        test)
            run_tests
            ;;
        stop)
            stop_services
            ;;
        clean)
            clean_all
            ;;
        status)
            show_status
            ;;
        logs)
            show_logs
            ;;
        help|--help|-h)
            show_help
            ;;
        *)
            log_error "Unknown command: $1"
            show_help
            exit 1
            ;;
    esac
}

main "$@"
