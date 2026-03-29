#!/bin/bash
# XBOOT Repeated Smoke Test (Soak Test)
# Runs smoke tests repeatedly to detect intermittent failures and guest protocol drift

set -uo pipefail

API_KEY="${1:-}"
BASE_URL="${2:-http://127.0.0.1:8080}"
ITERATIONS="${3:-100}"
CONCURRENCY="${4:-1}"

if [[ -z "$API_KEY" ]]; then
    echo "Usage: $0 <api_key> [base_url] [iterations] [concurrency]"
    echo "Example: $0 test-key http://127.0.0.1:8080 100 1"
    echo ""
    echo "Arguments:"
    echo "  api_key      - API key for authentication"
    echo "  base_url     - Base URL (default: http://127.0.0.1:8080)"
    echo "  iterations   - Number of test iterations (default: 100)"
    echo "  concurrency  - Parallel requests (default: 1, use with care)"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SMOKE_SCRIPT="$SCRIPT_DIR/smoke_exec.sh"

if [[ ! -x "$SMOKE_SCRIPT" ]]; then
    echo "Error: smoke_exec.sh not found or not executable at $SMOKE_SCRIPT"
    exit 1
fi

PYTHON_FAILURES=0
NODE_FAILURES=0
LIVE_FAILURES=0
READY_FAILURES=0
TOTAL_ATTEMPTS=0
START_TIME=$(date +%s)

echo "=== XBOOT Repeated Smoke Test ==="
echo "Base URL: $BASE_URL"
echo "Iterations: $ITERATIONS"
echo "Concurrency: $CONCURRENCY"
echo "Start time: $(date -Iseconds)"
echo ""

# Create temp directory for parallel execution
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

run_single_test() {
    local iter="$1"
    local test_type="$2"
    local temp_file="$TEMP_DIR/result_${iter}_${test_type}.txt"
    
    case "$test_type" in
        live)
            if curl -fsS "$BASE_URL/live" > /dev/null 2>&1; then
                echo "PASS" > "$temp_file"
            else
                echo "FAIL" > "$temp_file"
            fi
            ;;
        ready)
            if curl -fsS "$BASE_URL/ready" > /dev/null 2>&1; then
                echo "PASS" > "$temp_file"
            else
                echo "FAIL" > "$temp_file"
            fi
            ;;
        python)
            response=$(curl -fsS -X POST "$BASE_URL/v1/exec" \
                -H "Content-Type: application/json" \
                -H "Authorization: Bearer $API_KEY" \
                -d "{\"language\": \"python\", \"code\": \"print($iter + $iter)\", \"timeout_seconds\": 30}" \
                2>/dev/null || true)
            if [[ -n "$response" ]]; then
                stdout=$(echo "$response" | jq -r '.stdout // empty')
                if [[ "$stdout" == "$((iter + iter))" ]]; then
                    echo "PASS" > "$temp_file"
                else
                    echo "FAIL: stdout=$stdout" > "$temp_file"
                fi
            else
                echo "FAIL: no response" > "$temp_file"
            fi
            ;;
        node)
            response=$(curl -fsS -X POST "$BASE_URL/v1/exec" \
                -H "Content-Type: application/json" \
                -H "Authorization: Bearer $API_KEY" \
                -d "{\"language\": \"node\", \"code\": \"console.log($iter + $iter)\", \"timeout_seconds\": 30}" \
                2>/dev/null || true)
            if [[ -n "$response" ]]; then
                stdout=$(echo "$response" | jq -r '.stdout // empty')
                if [[ "$stdout" == "$((iter + iter))" ]]; then
                    echo "PASS" > "$temp_file"
                else
                    echo "FAIL: stdout=$stdout" > "$temp_file"
                fi
            else
                echo "FAIL: no response" > "$temp_file"
            fi
            ;;
    esac
}

# Run tests
for i in $(seq 1 $ITERATIONS); do
    if [[ $CONCURRENCY -gt 1 ]]; then
        # Parallel execution
        for j in $(seq 1 $CONCURRENCY); do
            run_single_test "$i" "live" &
            run_single_test "$i" "ready" &
            run_single_test "$i" "python" &
            run_single_test "$i" "node" &
        done
        wait
    else
        # Sequential execution with detailed output
        if [ $((i % 10)) -eq 1 ] || [[ "$i" -eq 1 ]]; then
            echo "Iteration $i/$ITERATIONS..."
        fi
        
        # Test /live
        if ! curl -fsS "$BASE_URL/live" > /dev/null 2>&1; then
            echo "  FAIL: /live at iteration $i"
            LIVE_FAILURES=$((LIVE_FAILURES + 1))
        fi
        
        # Test /ready
        if ! curl -fsS "$BASE_URL/ready" > /dev/null 2>&1; then
            echo "  FAIL: /ready at iteration $i"
            READY_FAILURES=$((READY_FAILURES + 1))
        fi
        
        # Test Python exec
        py_response=$(curl -fsS -X POST "$BASE_URL/v1/exec" \
            -H "Content-Type: application/json" \
            -H "Authorization: Bearer $API_KEY" \
            -d "{\"language\": \"python\", \"code\": \"print($i)\", \"timeout_seconds\": 30}" \
            2>/dev/null || true)
        if [[ -n "$py_response" ]]; then
            py_stdout=$(echo "$py_response" | jq -r '.stdout // empty')
            py_error=$(echo "$py_response" | jq -r '.error // empty')
            if [[ "$py_stdout" != "$i" ]]; then
                echo "  FAIL: Python at iteration $i (expected $i, got stdout='$py_stdout' error='$py_error')"
                PYTHON_FAILURES=$((PYTHON_FAILURES + 1))
            fi
        else
            echo "  FAIL: Python at iteration $i (no response)"
            PYTHON_FAILURES=$((PYTHON_FAILURES + 1))
        fi
        
        # Test Node.js exec
        node_response=$(curl -fsS -X POST "$BASE_URL/v1/exec" \
            -H "Content-Type: application/json" \
            -H "Authorization: Bearer $API_KEY" \
            -d "{\"language\": \"node\", \"code\": \"console.log($i)\", \"timeout_seconds\": 30}" \
            2>/dev/null || true)
        if [[ -n "$node_response" ]]; then
            node_stdout=$(echo "$node_response" | jq -r '.stdout // empty')
            node_error=$(echo "$node_response" | jq -r '.error // empty')
            if [[ "$node_stdout" != "$i" ]]; then
                echo "  FAIL: Node.js at iteration $i (expected $i, got stdout='$node_stdout' error='$node_error')"
                NODE_FAILURES=$((NODE_FAILURES + 1))
            fi
        else
            echo "  FAIL: Node.js at iteration $i (no response)"
            NODE_FAILURES=$((NODE_FAILURES + 1))
        fi
        
        TOTAL_ATTEMPTS=$((TOTAL_ATTEMPTS + 4))
    fi
done

END_TIME=$(date +%s)
DURATION=$((END_TIME - START_TIME))

# Calculate results
if [[ $CONCURRENCY -gt 1 ]]; then
    # Aggregate parallel results
    for f in "$TEMP_DIR"/result_*.txt; do
        if [[ -f "$f" ]]; then
            content=$(cat "$f")
            if [[ "$content" != "PASS" ]]; then
                if [[ "$f" == *"_live.txt" ]]; then
                    LIVE_FAILURES=$((LIVE_FAILURES + 1))
                elif [[ "$f" == *"_ready.txt" ]]; then
                    READY_FAILURES=$((READY_FAILURES + 1))
                elif [[ "$f" == *"_python.txt" ]]; then
                    PYTHON_FAILURES=$((PYTHON_FAILURES + 1))
                elif [[ "$f" == *"_node.txt" ]]; then
                    NODE_FAILURES=$((NODE_FAILURES + 1))
                fi
            fi
        fi
    done
    TOTAL_ATTEMPTS=$((ITERATIONS * CONCURRENCY * 4))
fi

echo ""
echo "=== Results ==="
echo "Duration: ${DURATION}s"
echo "Total attempts: $TOTAL_ATTEMPTS"
echo ""
echo "Failures by endpoint:"
echo "  /live:      $LIVE_FAILURES"
echo "  /ready:     $READY_FAILURES"
echo "  Python:     $PYTHON_FAILURES"
echo "  Node.js:    $NODE_FAILURES"
echo ""

TOTAL_FAILURES=$((LIVE_FAILURES + READY_FAILURES + PYTHON_FAILURES + NODE_FAILURES))
if [[ $TOTAL_ATTEMPTS -gt 0 ]]; then
    SUCCESS_RATE=$((100 * (TOTAL_ATTEMPTS - TOTAL_FAILURES) / TOTAL_ATTEMPTS))
    echo "Success rate: ${SUCCESS_RATE}%"
fi

echo ""
if [[ $TOTAL_FAILURES -gt 0 ]]; then
    echo "=== UNSTABLE: Guest protocol drift or intermittent failures detected ==="
    echo "DO NOT PROCEED TO DOCKER/KUBERNETES UNTIL FIXED"
    exit 1
else
    echo "=== STABLE: No protocol drift detected ==="
    echo "Host path is ready for Phase B (Docker packaging)"
    exit 0
fi
