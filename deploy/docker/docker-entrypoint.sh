#!/bin/bash
# XBOOT Docker Entrypoint
# Handles startup verification before running zeroboot

set -euo pipefail

# Configuration
RELEASE_ROOT="${RELEASE_ROOT:-/var/lib/zeroboot/current}"
TEMPLATE_DIRS="${TEMPLATE_DIRS:-python:${RELEASE_ROOT}/templates/python,node:${RELEASE_ROOT}/templates/node}"
VERIFY_ON_STARTUP="${VERIFY_ON_STARTUP:-true}"

# Logging helpers
log_info() {
    echo "[entrypoint] INFO: $*"
}

log_error() {
    echo "[entrypoint] ERROR: $*" >&2
}

log_fatal() {
    echo "[entrypoint] FATAL: $*" >&2
    exit 1
}

# Check required devices
check_devices() {
    log_info "Checking device access..."
    
    if [[ ! -e /dev/kvm ]]; then
        log_fatal "/dev/kvm not found. KVM is required for XBOOT."
    fi
    
    if [[ ! -r /dev/kvm ]]; then
        log_fatal "/dev/kvm not readable. Check permissions."
    fi
    
    if [[ ! -w /dev/kvm ]]; then
        log_fatal "/dev/kvm not writable. Check permissions."
    fi
    
    log_info "KVM device access OK"
}

# Check cgroup v2
check_cgroup() {
    log_info "Checking cgroup configuration..."
    
    if [[ ! -f /sys/fs/cgroup/cgroup.controllers ]]; then
        log_fatal "cgroup v2 not detected. XBOOT requires cgroup v2."
    fi
    
    log_info "cgroup v2 OK"
}

# Check required binaries
check_binaries() {
    log_info "Checking binaries..."
    
    if ! command -v firecracker >/dev/null 2>&1; then
        log_fatal "firecracker not found in PATH"
    fi
    
    local fc_version
    fc_version=$(firecracker --version 2>/dev/null || echo "unknown")
    log_info "Firecracker version: $fc_version"
    
    if [[ ! -x "${RELEASE_ROOT}/bin/zeroboot" ]]; then
        log_fatal "zeroboot binary not found at ${RELEASE_ROOT}/bin/zeroboot"
    fi
    
    log_info "Binaries OK"
}

# Check templates exist
check_templates() {
    log_info "Checking templates..."
    
    # Parse template directories from TEMPLATE_DIRS
    IFS=',' read -ra TEMPLATES <<< "$TEMPLATE_DIRS"
    for template in "${TEMPLATES[@]}"; do
        IFS=':' read -r lang dir <<< "$template"
        
        if [[ ! -d "$dir" ]]; then
            log_fatal "Template directory not found: $dir (language: $lang)"
        fi
        
        if [[ ! -f "$dir/template.manifest.json" ]]; then
            log_fatal "Template manifest not found: $dir/template.manifest.json"
        fi
        
        log_info "Template OK: $lang -> $dir"
    done
}

# Run verify-startup
do_verify_startup() {
    if [[ "$VERIFY_ON_STARTUP" != "true" ]]; then
        log_info "Skipping verify-startup (VERIFY_ON_STARTUP=$VERIFY_ON_STARTUP)"
        return 0
    fi
    
    log_info "Running verify-startup..."
    
    local verify_result
    if "${RELEASE_ROOT}/bin/zeroboot" verify-startup "$TEMPLATE_DIRS" --release-root "$RELEASE_ROOT" 2>&1; then
        verify_result=$?
    else
        verify_result=$?
    fi
    
    if [[ $verify_result -ne 0 ]]; then
        log_fatal "verify-startup failed with exit code $verify_result"
    fi
    
    log_info "verify-startup passed"
}

# Handle signals for graceful shutdown
setup_signal_handling() {
    shutdown_handler() {
        log_info "Received shutdown signal, exiting..."
        exit 0
    }
    
    trap shutdown_handler SIGTERM SIGINT
}

# Main entrypoint logic
main() {
    log_info "XBOOT Docker Entrypoint starting..."
    log_info "Release root: $RELEASE_ROOT"
    log_info "Templates: $TEMPLATE_DIRS"
    
    # Setup signal handling
    setup_signal_handling
    
    # Run checks
    check_devices
    check_cgroup
    check_binaries
    check_templates
    
    # Run startup verification
    do_verify_startup
    
    log_info "All checks passed. Starting zeroboot..."
    
    # If no arguments provided, default to serve mode
    if [[ $# -eq 0 ]]; then
        set -- serve "$TEMPLATE_DIRS" 8080
    fi
    
    # Execute zeroboot with provided arguments
    log_info "Executing: ${RELEASE_ROOT}/bin/zeroboot $*"
    exec "${RELEASE_ROOT}/bin/zeroboot" "$@"
}

# Run main
main "$@"
