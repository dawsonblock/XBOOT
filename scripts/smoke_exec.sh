#!/bin/bash
# XBOOT Basic Smoke Test
# Tests /live, /ready, /health endpoints and Python/Node.js exec

set -euo pipefail

API_KEY="${1:-}"
BASE_URL="${2:-http://127.0.0.1:8080}"

if [[ -z "$API_KEY" ]]; then
    echo "Usage: $0 <api_key> [base_url]"
    echo "Example: $0 test-key http://127.0.0.1:8080"
    exit 1
fi

echo "=== XBOOT Smoke Test ==="
echo "Base URL: $BASE_URL"
echo ""

# Check if jq is available
if ! command -v jq >/dev/null 2>&1; then
    echo "Error: jq is required for JSON parsing"
    exit 1
fi

# Test /live endpoint
echo -n "Testing /live... "
if curl -fsS "$BASE_URL/live" > /dev/null 2>&1; then
    echo "OK"
else
    echo "FAIL"
    exit 1
fi

# Test /ready endpoint
echo -n "Testing /ready... "
if curl -fsS "$BASE_URL/ready" > /dev/null 2>&1; then
    echo "OK"
else
    echo "FAIL"
    exit 1
fi

# Test /health endpoint
echo -n "Testing /health... "
health_status=$(curl -fsS "$BASE_URL/health" 2>/dev/null | jq -r '.status // "unknown"')
if [[ "$health_status" == "ok" ]]; then
    echo "OK (status: $health_status)"
else
    echo "FAIL (status: $health_status)"
    exit 1
fi

# Test Python exec
echo -n "Testing Python exec... "
python_response=$(curl -fsS -X POST "$BASE_URL/v1/exec" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $API_KEY" \
    -d '{"language": "python", "code": "print(2+2)", "timeout_seconds": 30}' \
    2>/dev/null || true)

if [[ -n "$python_response" ]]; then
    python_stdout=$(echo "$python_response" | jq -r '.stdout // empty')
    python_error=$(echo "$python_response" | jq -r '.error // empty')
    
    if [[ "$python_stdout" == "4" ]]; then
        echo "OK (output: $python_stdout)"
    elif [[ -n "$python_error" ]]; then
        echo "FAIL (error: $python_error)"
        exit 1
    else
        echo "FAIL (unexpected output: $python_stdout)"
        exit 1
    fi
else
    echo "FAIL (no response)"
    exit 1
fi

# Test Node.js exec
echo -n "Testing Node.js exec... "
node_response=$(curl -fsS -X POST "$BASE_URL/v1/exec" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $API_KEY" \
    -d '{"language": "node", "code": "console.log(2+2)", "timeout_seconds": 30}' \
    2>/dev/null || true)

if [[ -n "$node_response" ]]; then
    node_stdout=$(echo "$node_response" | jq -r '.stdout // empty')
    node_error=$(echo "$node_response" | jq -r '.error // empty')
    
    if [[ "$node_stdout" == "4" ]]; then
        echo "OK (output: $node_stdout)"
    elif [[ -n "$node_error" ]]; then
        echo "FAIL (error: $node_error)"
        exit 1
    else
        echo "FAIL (unexpected output: $node_stdout)"
        exit 1
    fi
else
    echo "FAIL (no response)"
    exit 1
fi

# Test unsupported language fails closed
echo -n "Testing unsupported language rejection... "
ruby_response=$(curl -fsS -X POST "$BASE_URL/v1/exec" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $API_KEY" \
    -d '{"language": "ruby", "code": "puts 1+1"}' \
    2>/dev/null || true)

if [[ -n "$ruby_response" ]]; then
    ruby_error=$(echo "$ruby_response" | jq -r '.error // empty')
    if [[ "$ruby_error" == *"unsupported"* ]] || [[ "$ruby_error" == *"Unsupported"* ]] || [[ "$ruby_error" == *"language"* ]]; then
        echo "OK (rejected: $ruby_error)"
    else
        echo "WARN (unexpected response: $ruby_response)"
    fi
else
    echo "WARN (no response for unsupported language)"
fi

echo ""
echo "=== All smoke tests passed ==="
