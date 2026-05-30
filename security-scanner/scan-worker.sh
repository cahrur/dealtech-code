#!/bin/bash
# Security Scan Worker
# Runs on HOST (not in container) to avoid competing with backend RAM.
# Single-threaded: processes one scan at a time. Others wait in Redis queue.
# Managed by systemd: security-scan-worker.service

set -uo pipefail

REDIS_CLI="docker exec -i ai-platform-redis-1 redis-cli"
TEMPLATES_DIR="/root/nuclei-templates/http"
SCAN_QUEUE="scan:queue"
RUNNING_KEY="scan:running"
LOG_FILE="/var/log/scan-worker.log"

log() {
    # systemd StandardOutput already appends stdout to LOG_FILE; just echo once
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1"
}

# Template dirs per mode (swap covers the memory; no batching needed)
template_args() {
    case "$1" in
        quick)     echo "-t $TEMPLATES_DIR/exposures -t $TEMPLATES_DIR/misconfiguration" ;;
        full)      echo "-t $TEMPLATES_DIR" ;;
        recon)     echo "-t $TEMPLATES_DIR/technologies" ;;
        cves)      echo "-t $TEMPLATES_DIR/cves" ;;
        misconfig) echo "-t $TEMPLATES_DIR/misconfiguration" ;;
        exposure)  echo "-t $TEMPLATES_DIR/exposures -t $TEMPLATES_DIR/exposed-panels" ;;
        *)         echo "-t $TEMPLATES_DIR/exposures -t $TEMPLATES_DIR/misconfiguration" ;;
    esac
}

# Active SQL-injection scan via sqlmap. Crawls + tests forms and URL params,
# then emits findings in the same JSON shape nuclei produces so the backend
# formatter handles both uniformly. sqlmap is light on RAM (~150-200MB).
run_sqli_scan() {
    local url="$1"
    local budget="$2"
    local outdir
    outdir=$(mktemp -d /tmp/sqlmap-XXXXXX)
    local logf="$outdir/run.log"

    # Smart mode: if the URL already has a query parameter (?x=y), test it
    # directly (fast, ~10-30s). Otherwise auto-crawl the site to discover
    # injectable URLs/forms on its own (no manual parameter needed, ~1-3 min).
    local extra_args
    if printf '%s' "$url" | grep -q '?[^=]*='; then
        extra_args="--level=2 --risk=1"
    else
        extra_args="--crawl=2 --forms --level=1 --risk=1"
    fi

    timeout -k 30 "$budget" sqlmap -u "$url" \
        --batch $extra_args \
        --random-agent --timeout=10 --retries=1 --threads=5 \
        --flush-session \
        --output-dir="$outdir" >"$logf" 2>&1 || true

    # Parse sqlmap's injection-point summary into JSONL findings.
    awk -v url="$url" '
        /sqlmap identified the following injection point/ {insum=1}
        insum && /^Parameter:/ {
            if (param != "") emit();
            param=$0; sub(/^Parameter: */,"",param); types="";
        }
        insum && /Type:/ {
            t=$0; sub(/^[ \t]*Type: */,"",t);
            types = (types=="" ? t : types "; " t);
        }
        /back-end DBMS:/ { dbms=$0; sub(/.*back-end DBMS: */,"",dbms); }
        END { if (param != "") emit(); }
        function emit() {
            gsub(/\\/,"",param); gsub(/"/,"",param);
            gsub(/\\/,"",types); gsub(/"/,"",types);
            gsub(/\\/,"",dbms);  gsub(/"/,"",dbms);
            printf "{\"info\":{\"name\":\"SQL Injection - %s\",\"severity\":\"critical\",\"description\":\"Types: %s | DBMS: %s\"},\"matched-at\":\"%s\",\"template-id\":\"sqlmap-sqli\"}\n", param, types, dbms, url;
        }
    ' "$logf"

    rm -rf "$outdir"
}

log "Scan worker started"

while true; do
    # Block-wait for a job (30s timeout, then loop)
    # redis-cli output is raw when piped: line1=key, line2=value
    RAW=$($REDIS_CLI BRPOP "$SCAN_QUEUE" 30 2>/dev/null)
    [ -z "${RAW:-}" ] && continue
    JOB=$(echo "$RAW" | sed -n '2p')
    [ -z "${JOB:-}" ] && continue

    URL=$(echo "$JOB" | jq -r '.url // empty' 2>/dev/null)
    MODE=$(echo "$JOB" | jq -r '.mode // "quick"' 2>/dev/null)
    CHAT_ID=$(echo "$JOB" | jq -r '.chat_id // empty' 2>/dev/null)

    if [ -z "$URL" ] || [ -z "$CHAT_ID" ]; then
        log "Invalid job skipped: $JOB"
        continue
    fi

    # Mark globally running (used by backend to show queue status)
    $REDIS_CLI SET "$RUNNING_KEY" "$CHAT_ID" EX 3600 >/dev/null 2>&1

    log "Scan start: url=$URL mode=$MODE chat_id=$CHAT_ID"

    SEVERITY="low,medium,high,critical"
    case "$MODE" in
        recon|full) SEVERITY="info,low,medium,high,critical" ;;
    esac

    TARGS=$(template_args "$MODE")

    SCAN_BUDGET=900  # 15 minutes
    [ "$MODE" = "full" ] && SCAN_BUDGET=1800  # 30 minutes for full scans

    if [ "$MODE" = "sqli" ]; then
        # Active SQL-injection testing via sqlmap (separate code path)
        RESULT=$(run_sqli_scan "$URL" "$SCAN_BUDGET")
    else
        # Signature-based scanning via nuclei. Hard wall-clock budget so a
        # throttling/slow target cannot hang the single-scan queue.
        RESULT=$(timeout -k 30 "$SCAN_BUDGET" nuclei -u "$URL" $TARGS \
            -severity "$SEVERITY" \
            -silent -no-color -jsonl -omit-raw \
            -c 25 -rl 150 -timeout 10 -retries 1 \
            2>/dev/null || true)
    fi

    # Build result JSON
    if [ -z "$RESULT" ]; then
        RESULT_JSON='{"status":"done","findings":[]}'
    else
        FINDINGS=$(echo "$RESULT" | grep -v '^$' | jq -s '.' 2>/dev/null || echo "[]")
        RESULT_JSON=$(jq -n --argjson f "$FINDINGS" '{status:"done",findings:$f}' 2>/dev/null || echo '{"status":"done","findings":[]}')
    fi

    # Publish result + clear markers
    # NOTE: redis-cli -x appends stdin as the LAST arg, so set value first, then EXPIRE
    echo "$RESULT_JSON" | $REDIS_CLI -x SET "scan:result:${CHAT_ID}" >/dev/null 2>&1
    $REDIS_CLI EXPIRE "scan:result:${CHAT_ID}" 3600 >/dev/null 2>&1
    $REDIS_CLI DEL "$RUNNING_KEY" >/dev/null 2>&1
    # Signal the backend deliverer (persists across backend restarts)
    $REDIS_CLI LPUSH "scan:delivery" "${CHAT_ID}" >/dev/null 2>&1

    FCOUNT=$(echo "$RESULT" | grep -vc '^$' 2>/dev/null || echo 0)
    log "Scan done: url=$URL findings=$FCOUNT"
done
