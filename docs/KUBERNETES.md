# XBOOT Kubernetes Deployment Guide

Phase C - Kubernetes deployment for the zeroboot runtime.

## Overview

This guide covers deploying XBOOT on Kubernetes. XBOOT runs as an **infrastructure workload** requiring:
- Privileged pods (for KVM and cgroup access)
- `/dev/kvm` device exposure
- Dedicated KVM-capable nodes
- Node affinity and taints/tolerations for isolation

**Prerequisites:** Phase A (bare metal) and Phase B (Docker) must be complete and stable.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Kubernetes Cluster                          │
│                                                                     │
│  ┌─────────────────────────────────────────────────────────────┐   │
│  │  Dedicated KVM Node Pool (labeled: sandbox.kvm=true)        │   │
│  │                                                             │   │
│  │  ┌─────────────────┐  ┌─────────────────┐                   │   │
│  │  │   Node 1        │  │   Node 2        │  ...             │   │
│  │  │  ┌───────────┐  │  │  ┌───────────┐  │                   │   │
│  │  │  │  xboot    │  │  │  │  xboot    │  │                   │   │
│  │  │  │  Pod      │  │  │  │  Pod      │  │                   │   │
│  │  │  │ ┌───────┐ │  │  │  │ ┌───────┐ │  │                   │   │
│  │  │  │ │zeroboot│ │  │  │  │ │zeroboot│ │  │                   │   │
│  │  │  │ │+FC    │ │  │  │  │ │+FC    │ │  │                   │   │
│  │  │  │ └───────┘ │  │  │  │ └───────┘ │  │                   │   │
│  │  │  │privileged│  │  │  │ │privileged│  │                   │   │
│  │  │  └───────────┘  │  │  │  └───────────┘  │                   │   │
│  │  │       │         │  │  │       │         │                   │   │
│  │  └───────┼─────────┘  │  │  └───────┼─────────┘                   │   │
│  │          │            │  │          │                             │   │
│  │          └────────────┴──┴──────────┘                             │   │
│  │                    Service: xboot                                  │   │
│  │                                                                     │   │
│  │  /dev/kvm ────────────────────────────┐                            │   │
│  │  cgroup v2 ─────────────────────────┘ (node-level)                 │   │
│  │                                                                     │   │
│  │  Taint: sandbox.kvm=true:NoSchedule (keeps other workloads away)  │   │
│  │                                                                     │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  (Other nodes cannot run xboot pods due to node affinity)                  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Quick Start

### 1. Prepare KVM Nodes

Label and taint your KVM-capable nodes:

```bash
# For each KVM node
kubectl label node <node-name> sandbox.kvm=true
kubectl taint node <node-name> sandbox.kvm=true:NoSchedule
```

### 2. Create Namespace and Secrets

```bash
# Create namespace
kubectl apply -f deploy/k8s/namespace.yaml

# Create secrets (edit secret-example.yaml first!)
# Generate secrets:
python3 scripts/make_api_keys.py --count 1 --out secrets/api_keys.json
openssl rand -hex 32 > secrets/pepper

# Apply secrets
kubectl create secret generic xboot-secrets \
  --from-file=api_keys.json=secrets/api_keys.json \
  --from-file=pepper=secrets/pepper \
  -n xboot
```

### 3. Create Storage

```bash
# Create PVCs for release and state
kubectl apply -f deploy/k8s/pvc-release.yaml
kubectl apply -f deploy/k8s/pvc-state.yaml

# Alternative for DaemonSet: pre-populate node storage
# On each node:
mkdir -p /var/lib/zeroboot/current /var/lib/zeroboot/state
# Copy release tree to /var/lib/zeroboot/current/
```

### 4. Deploy XBOOT

Using Kustomize (recommended):

```bash
# Set version
export VERSION=latest

# Apply with kustomize
kubectl apply -k deploy/k8s/
```

Or apply individual files:

```bash
kubectl apply -f deploy/k8s/deployment.yaml
kubectl apply -f deploy/k8s/service.yaml
kubectl apply -f deploy/k8s/networkpolicy.yaml
```

### 5. Verify Deployment

```bash
# Check pods
kubectl get pods -n xboot -o wide

# Check service
kubectl get svc -n xboot

# Check logs
kubectl logs -n xboot -l app.kubernetes.io/name=xboot

# Port forward for testing
kubectl port-forward -n xboot svc/xboot 8080:8080

# Run smoke test
./scripts/smoke_exec.sh <api-key> http://127.0.0.1:8080
```

## Deployment Options

### Option 1: Deployment (1-3 replicas)

Use `deploy/k8s/deployment.yaml` for a small number of replicas with PVC-backed storage.

**Best for:**
- Development/testing
- Small scale (1-3 pods)
- Shared storage model

```bash
kubectl apply -f deploy/k8s/deployment.yaml
```

### Option 2: DaemonSet (one per node)

Use `deploy/k8s/daemonset.yaml` for running one pod on every KVM node.

**Best for:**
- Production fleet
- Node-local storage
- Maximum isolation

```bash
kubectl apply -f deploy/k8s/daemonset.yaml
```

**DaemonSet requirements:**
- Release tree at `/var/lib/zeroboot/current` on each node
- State directory at `/var/lib/zeroboot/state` on each node

## Configuration

### Node Setup

Each KVM node needs:

1. **KVM support**:
   ```bash
   test -e /dev/kvm && test -r /dev/kvm && test -w /dev/kvm
   ```

2. **cgroup v2**:
   ```bash
   test -e /sys/fs/cgroup/cgroup.controllers
   ```

3. **Labels and taints**:
   ```bash
   kubectl label node <node> sandbox.kvm=true
   kubectl taint node <node> sandbox.kvm=true:NoSchedule
   ```

4. **Release tree** (for DaemonSet):
   ```bash
   mkdir -p /var/lib/zeroboot/current
   # Copy or mount release tree here
   ```

### Secrets Management

#### Option A: kubectl create (development)

```bash
kubectl create secret generic xboot-secrets \
  --from-file=api_keys.json=secrets/api_keys.json \
  --from-file=pepper=secrets/pepper \
  -n xboot
```

#### Option B: Sealed Secrets (production)

```bash
# Install sealed-secrets
helm install sealed-secrets sealed-secrets/sealed-secrets

# Encrypt your secret
kubeseal --format yaml < secret.yaml > sealed-secret.yaml

# Apply sealed secret (safe for git)
kubectl apply -f sealed-secret.yaml
```

#### Option C: External Secrets Operator

```bash
# Use AWS Secrets Manager, Azure Key Vault, etc.
kubectl apply -f deploy/k8s/external-secret.yaml
```

### Resource Limits

Adjust in deployment/daemonset:

```yaml
resources:
  requests:
    memory: "1Gi"
    cpu: "500m"
  limits:
    memory: "4Gi"  # Per-pod limit
    cpu: "2000m"
```

## Security

### Privileged Pods

XBOOT requires privileged pods for:
- KVM device access (`/dev/kvm`)
- cgroup v2 management
- Firecracker VM operations

This is expected for infrastructure workloads. Mitigations:

1. **Node isolation**: KVM nodes only run xboot pods (taints)
2. **Network policies**: Restrict ingress/egress
3. **Read-only root**: Container filesystem is read-only
4. **Secrets mounted read-only**: API keys, pepper

### Network Policy

Default policy restricts access:

```bash
kubectl apply -f deploy/k8s/networkpolicy.yaml
```

Allow from:
- Ingress controllers
- Monitoring namespace
- Same namespace only

### Pod Security

Namespace has privileged enforcement:

```yaml
labels:
  pod-security.kubernetes.io/enforce: privileged
  pod-security.kubernetes.io/audit: privileged
  pod-security.kubernetes.io/warn: privileged
```

## Monitoring

### Health Checks

- **Liveness**: `/live` - lightweight, fast
- **Readiness**: `/ready` - checks templates loaded
- **Startup**: `verify-startup` runs before container start

### Metrics

Scrape `/v1/metrics` from Prometheus:

```yaml
# Prometheus ServiceMonitor
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: xboot
  namespace: monitoring
spec:
  selector:
    matchLabels:
      app.kubernetes.io/name: xboot
  endpoints:
  - port: http
    path: /v1/metrics
    interval: 30s
```

### Logging

Logs are written to:
- Container stdout (structured)
- Request log: `/var/lib/zeroboot/requests.jsonl`

Ship with Fluentd/Fluent Bit:

```yaml
# DaemonSet to collect logs
volumes:
- name: xboot-logs
  hostPath:
    path: /var/lib/zeroboot/state
```

## Troubleshooting

### Pod stuck in Pending

Check node affinity:

```bash
kubectl describe pod -n xboot <pod-name>
# Look for: 0/3 nodes are available: 3 node(s) didn't match Pod's node affinity

# Fix: Label and taint nodes
kubectl label node <node> sandbox.kvm=true
kubectl taint node <node> sandbox.kvm=true:NoSchedule
```

### verify-startup fails

Check logs:

```bash
kubectl logs -n xboot <pod-name>
```

Common causes:
- Templates not found (check PVC mounts)
- Secrets missing (check secret creation)
- KVM not available (check node labels)
- Firecracker version mismatch

### Guest protocol drift

Run soak test:

```bash
# Port forward
kubectl port-forward -n xboot svc/xboot 8080:8080 &

# Run repeated smoke test
./scripts/repeat_smoke.sh <api-key> http://127.0.0.1:8080 500
```

If unstable, **DO NOT SCALE**. Fix the host path first.

## Scaling

### Horizontal Scaling

Only scale if soak tests pass consistently:

```bash
# For Deployment
kubectl scale deployment xboot -n xboot --replicas=3

# For DaemonSet (add more nodes)
# Provision new KVM nodes, label and taint them
```

### Vertical Scaling

Adjust resource limits per pod:

```bash
kubectl patch deployment xboot -n xboot --type='json' -p='[{
  "op": "replace",
  "path": "/spec/template/spec/containers/0/resources/limits/memory",
  "value": "8Gi"
}]'
```

## Rollouts and Updates

### Rolling Update

```bash
# Update image
kubectl set image deployment/xboot xboot=xboot-runtime:v0.2.0 -n xboot

# Watch rollout
kubectl rollout status deployment/xboot -n xboot

# Rollback if needed
kubectl rollout undo deployment/xboot -n xboot
```

### Canary Deployment

```bash
# Create canary deployment with 1 replica
kubectl apply -f deploy/k8s/deployment-canary.yaml

# Test canary
# If successful, promote to main deployment
kubectl scale deployment xboot-canary -n xboot --replicas=0
kubectl set image deployment/xboot xboot=xboot-runtime:v0.2.0 -n xboot
```

## Cleanup

```bash
# Remove all resources
kubectl delete -k deploy/k8s/

# Or individually
kubectl delete -f deploy/k8s/deployment.yaml
kubectl delete -f deploy/k8s/service.yaml
kubectl delete -f deploy/k8s/pvc-state.yaml
kubectl delete -f deploy/k8s/pvc-release.yaml
kubectl delete -f deploy/k8s/secret-example.yaml
kubectl delete -f deploy/k8s/namespace.yaml
```

## Acceptance Criteria

Kubernetes Phase C is complete when:

- [x] Pods schedule only on labeled/tainted KVM nodes
- [x] Non-KVM nodes cannot schedule xboot pods
- [x] `/live` and `/ready` are green on all pods
- [x] Exec smoke works from inside cluster
- [x] Service routing works for all replicas
- [x] Node drain and reschedule work without data loss
- [x] Rolling update succeeds without downtime
- [x] Rollback from bad version works cleanly
- [x] No intermittent guest protocol failures in soak tests

## Production Checklist

- [ ] Use Sealed Secrets or External Secrets Operator
- [ ] Set up Prometheus metrics scraping
- [ ] Configure log aggregation
- [ ] Set up alerting for failed pods
- [ ] Test node drain procedure
- [ ] Document rollback procedure
- [ ] Run extended soak tests (1000+ iterations)
- [ ] Load test with realistic request patterns
- [ ] Set up PodDisruptionBudget
- [ ] Configure network policies
- [ ] Enable audit logging

## Files Reference

| File | Purpose |
|------|---------|
| `deploy/k8s/namespace.yaml` | Namespace with privileged policy |
| `deploy/k8s/secret-example.yaml` | Secrets template |
| `deploy/k8s/pvc-release.yaml` | Release volume claim |
| `deploy/k8s/pvc-state.yaml` | State volume claim |
| `deploy/k8s/deployment.yaml` | Deployment for 1-3 replicas |
| `deploy/k8s/daemonset.yaml` | DaemonSet for fleet deployment |
| `deploy/k8s/service.yaml` | Cluster service |
| `deploy/k8s/networkpolicy.yaml` | Network isolation |
| `deploy/k8s/kustomization.yaml` | Kustomize base configuration |
