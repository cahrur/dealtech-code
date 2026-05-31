#!/bin/bash
# Security Scan Worker
# Runs on HOST (not in container) to avoid competing with backend RAM.
# Single-threaded: processes one scan at a time. Others wait in Redis queue.
# Managed by systemd: security-scan-worker.service

set -uo pipefail

# katana (used by xss auto-crawl) aborts with "could not get home directory:
# $HOME is not defined" when launched by systemd (which provides no HOME).
# Define it defensively so the crawler works regardless of launch context.
export HOME="${HOME:-/root}"


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

# --- Authenticated-scan support ----------------------------------------
# /setauth stores cookie/bearer/header creds; the backend forwards them in the
# job as .auth.{kind,value}. We translate that into the right flags per tool.
# Values may contain spaces/;/quotes, so we use bash ARRAYS to avoid word-
# splitting / command injection. SECURITY: never log the value itself.
NUCLEI_AUTH=(); KATANA_AUTH=(); DALFOX_AUTH=(); SQLMAP_AUTH=()
build_auth_args() {
    NUCLEI_AUTH=(); KATANA_AUTH=(); DALFOX_AUTH=(); SQLMAP_AUTH=()
    [ -z "${AUTH_KIND:-}" ] && return
    case "$AUTH_KIND" in
        cookie)
            NUCLEI_AUTH=(-H "Cookie: $AUTH_VALUE")
            KATANA_AUTH=(-H "Cookie: $AUTH_VALUE")
            DALFOX_AUTH=(--cookie "$AUTH_VALUE")
            SQLMAP_AUTH=(--cookie="$AUTH_VALUE")
            ;;
        bearer)
            NUCLEI_AUTH=(-H "Authorization: Bearer $AUTH_VALUE")
            KATANA_AUTH=(-H "Authorization: Bearer $AUTH_VALUE")
            DALFOX_AUTH=(--header "Authorization: Bearer $AUTH_VALUE")
            SQLMAP_AUTH=(--headers="Authorization: Bearer $AUTH_VALUE")
            ;;
        header)
            # value is already in "Name: Value" form
            NUCLEI_AUTH=(-H "$AUTH_VALUE")
            KATANA_AUTH=(-H "$AUTH_VALUE")
            DALFOX_AUTH=(--header "$AUTH_VALUE")
            SQLMAP_AUTH=(--headers="$AUTH_VALUE")
            ;;
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
        "${SQLMAP_AUTH[@]}" \
        --random-agent --timeout=10 --retries=1 --threads=5 \
        --flush-session \
        --output-dir="$outdir" >"$logf" 2>&1 || true

    # Get the back-end DBMS (single global value) from the log.
    local dbms
    dbms=$(grep -m1 "back-end DBMS:" "$logf" 2>/dev/null | sed 's/.*back-end DBMS: *//')

    # sqlmap writes a results CSV listing EVERY vulnerable URL+param it found
    # (including the real injectable endpoint discovered during --crawl).
    local csv
    csv=$(find "$outdir" -name "results-*.csv" 2>/dev/null | head -1)

    if [ -n "$csv" ] && [ -s "$csv" ]; then
        awk -F',' -v dbms="$dbms" '
            NR==1 { next }
            NF>=3 {
                turl=$1; place=$2; param=$3; tech=$4;
                full="";
                for (i=1;i<=length(tech);i++){
                    c=substr(tech,i,1);
                    if (c=="B") nm="boolean-based blind";
                    else if (c=="E") nm="error-based";
                    else if (c=="U") nm="UNION query";
                    else if (c=="S") nm="stacked queries";
                    else if (c=="T") nm="time-based blind";
                    else if (c=="Q") nm="inline query";
                    else nm="";
                    if (nm!="") full=(full=="" ? nm : full "; " nm);
                }
                if (full=="") full="confirmed";
                gsub(/"/,"",param); gsub(/"/,"",turl); gsub(/"/,"",dbms);
                printf "{\"info\":{\"name\":\"SQL Injection - %s (%s)\",\"severity\":\"critical\",\"description\":\"Types: %s | DBMS: %s\"},\"matched-at\":\"%s\",\"template-id\":\"sqlmap-sqli\"}\n", param, place, full, dbms, turl;
            }
        ' "$csv"
    else
        # Fallback: parse the log summary (e.g. if CSV not produced).
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
    fi

    rm -rf "$outdir"
}

# --- Active XSS testing via dalfox --------------------------------------
# dalfox mines parameters from the page and injects real XSS payloads.
# Smart mode (like sqli): if the URL has a query parameter (?x=y), test it
# directly (fast). Otherwise auto-crawl the site with katana to discover
# parameterized URLs, then test them all via dalfox pipe mode -- so the user
# does NOT need to know which parameter is vulnerable.
# NOTE: dalfox v2.9.0 --format json is broken (emits [{}]), so we parse its
# reliable plain [POC] stdout instead. [V]=verified (DOM-triggered, high),
# [R]=reflected (medium).
run_xss_scan() {
    local url="$1"
    local budget="$2"
    local outdir logf targets crawl_budget xss_budget
    outdir=$(mktemp -d /tmp/dalfox-XXXXXX)
    logf="$outdir/out.log"
    targets="$outdir/targets.txt"

    if printf '%s' "$url" | grep -q '?[^=]*='; then
        # URL already has a parameter: test it directly (~30-90s).
        timeout -k 30 "$budget" dalfox url "$url" \
            "${DALFOX_AUTH[@]}" \
            --no-color --no-spinner --skip-bav \
            --timeout 15 --worker 30 --delay 50 \
            >"$logf" 2>&1 || true
    else
        # No parameter: crawl with katana to find injectable URLs, then pipe
        # them into dalfox. Split budget: up to 1/3 (max 240s) for crawling.
        crawl_budget=$(( budget / 3 )); [ "$crawl_budget" -gt 240 ] && crawl_budget=240
        xss_budget=$(( budget - crawl_budget )); [ "$xss_budget" -lt 60 ] && xss_budget=60

        timeout -k 15 "$crawl_budget" katana -u "$url" \
            "${KATANA_AUTH[@]}" \
            -d 2 -silent -nc 2>/dev/null \
            | grep -E '\?[^=]*=' | sort -u | head -50 > "$targets" || true

        if [ -s "$targets" ]; then
            timeout -k 30 "$xss_budget" dalfox pipe \
                "${DALFOX_AUTH[@]}" \
                --no-color --no-spinner --skip-bav \
                --timeout 15 --worker 30 --delay 50 \
                < "$targets" >"$logf" 2>&1 || true
        else
            # Crawl found no parameterized URLs; fall back to testing the URL
            # itself (dalfox still mines params from the page DOM/forms).
            timeout -k 30 "$xss_budget" dalfox url "$url" \
                "${DALFOX_AUTH[@]}" \
                --no-color --no-spinner --skip-bav \
                --timeout 15 --worker 30 --delay 50 \
                >"$logf" 2>&1 || true
        fi
    fi

    if [ -s "$logf" ]; then
        grep -E '^[[:space:]]*\[POC\]\[[VR]\]' "$logf" 2>/dev/null \
          | sed -E 's/^[[:space:]]*\[POC\]\[([^]]*)\]\[([^]]*)\]\[([^]]*)\] (.*)$/\1|\2|\3|\4/' \
          | awk -F'|' '
              { key=$1"|"$3; if (seen[key]++) next;
                if ($1=="V"){sev="high";gn="verified"} else {sev="medium";gn="reflected"}
                gsub(/\\/,"",$3); gsub(/"/,"",$3);
                gsub(/\\/,"",$4); gsub(/"/,"",$4);
                printf "{\"info\":{\"name\":\"XSS (%s) - %s\",\"severity\":\"%s\",\"description\":\"Method: %s | Type: %s\"},\"matched-at\":\"%s\",\"template-id\":\"dalfox-xss\"}\n", gn,$3,sev,$2,$3,$4;
              }'
    fi
    rm -rf "$outdir"
}

# --- TLS/SSL configuration audit via testssl.sh -------------------------
# Checks cert validity, weak ciphers/protocols (TLS 1.0/1.1, SSLv3),
# known flaws (Heartbleed, ROBOT, etc). Only LOW+ severities are kept.
run_tls_scan() {
    local url="$1"
    local budget="$2"
    local outdir jf
    outdir=$(mktemp -d /tmp/testssl-XXXXXX)
    jf="$outdir/out.json"

    timeout -k 30 "$budget" testssl.sh \
        --quiet --color 0 --jsonfile "$jf" \
        --severity LOW \
        --openssl-timeout 10 --socket-timeout 10 \
        -p -S -U \
        "$url" >/dev/null 2>&1 || true

    if [ -s "$jf" ]; then
        jq -c --arg url "$url" '
            (if type=="array" then . else [.] end)[]
            | select(.severity != null)
            | select(.severity | ascii_upcase | test("LOW|MEDIUM|HIGH|CRITICAL"))
            | {info:{
                  name:("TLS/SSL: " + (.id // "issue")),
                  severity:(.severity | ascii_downcase),
                  description:((.finding // "") | .[0:160])
               },
               "matched-at":$url,
               "template-id":("testssl-" + (.id // "tls"))}' "$jf" 2>/dev/null
    fi
    rm -rf "$outdir"
}

# --- Dependency / secret / IaC scan via trivy --------------------------
# Takes a GIT REPOSITORY URL (not an app URL). Finds: known-CVE dependencies
# (Cargo.lock, package.json, go.mod, requirements.txt, ...), secrets committed
# to the repo, and IaC misconfig (Dockerfile, compose, k8s). Covers OWASP A06
# (vulnerable components) + A08 (supply chain) + leaked credentials.
# NOTE: https git URL. Private repos need GITHUB_TOKEN exported in worker env.
# SECURITY: we deliberately DO NOT emit the matched secret value, only the
# rule id + file:line, so the report itself never leaks the credential.
run_deps_scan() {
    local url="$1"
    local budget="$2"
    local outdir jf
    outdir=$(mktemp -d /tmp/trivy-XXXXXX)
    jf="$outdir/out.json"

    # Cache/DB live under $HOME/.cache/trivy (HOME exported at top of script).
    timeout -k 30 "$budget" trivy repo "$url" \
        --scanners vuln,secret,misconfig \
        --severity LOW,MEDIUM,HIGH,CRITICAL \
        --format json --quiet --no-progress \
        --timeout "${budget}s" \
        -o "$jf" >/dev/null 2>&1 || true

    if [ -s "$jf" ]; then
        jq -c '
            def sev: (. // "UNKNOWN") | ascii_downcase
                     | if . == "unknown" then "info" else . end;
            (.Results // [])[] as $r
            | (
                ($r.Vulnerabilities // [])[]
                | {info:{
                      name:("CVE " + (.VulnerabilityID // "?") + " - " + (.PkgName // "pkg")),
                      severity:(.Severity | sev),
                      description:(((.Title // "Known vulnerability")
                                    + " | installed " + (.InstalledVersion // "?")
                                    + " | fixed " + (.FixedVersion // "none"))[0:200])},
                   "matched-at":(($r.Target // "?") + " -> " + (.PkgName // "")),
                   "template-id":("trivy-vuln-" + (.VulnerabilityID // "x"))}
              ),
              (
                ($r.Secrets // [])[]
                | {info:{
                      name:("Secret leak - " + (.Title // .RuleID // "secret")),
                      severity:(.Severity | sev),
                      description:("Rule: " + (.RuleID // "?") + " | Category: " + (.Category // "?"))},
                   "matched-at":(($r.Target // "?") + ":" + ((.StartLine // 0)|tostring)),
                   "template-id":("trivy-secret-" + (.RuleID // "x"))}
              ),
              (
                ($r.Misconfigurations // [])[]
                | {info:{
                      name:("Misconfig - " + (.Title // .ID // "issue")),
                      severity:(.Severity | sev),
                      description:(((.Description // .Message // "")|tostring)[0:200])},
                   "matched-at":($r.Target // "?"),
                   "template-id":("trivy-misconfig-" + (.ID // "x"))}
              )
        ' "$jf" 2>/dev/null
    fi
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
    # Authenticated-scan creds (optional). Never logged.
    AUTH_KIND=$(echo "$JOB" | jq -r '.auth.kind // empty' 2>/dev/null)
    AUTH_VALUE=$(echo "$JOB" | jq -r '.auth.value // empty' 2>/dev/null)
    build_auth_args

    if [ -z "$URL" ] || [ -z "$CHAT_ID" ]; then
        log "Invalid job skipped: $JOB"
        continue
    fi

    # Mark globally running (used by backend to show queue status)
    $REDIS_CLI SET "$RUNNING_KEY" "$CHAT_ID" EX 3600 >/dev/null 2>&1

    log "Scan start: url=$URL mode=$MODE chat_id=$CHAT_ID auth=$([ -n "${AUTH_KIND:-}" ] && echo "$AUTH_KIND" || echo none)"

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
    elif [ "$MODE" = "xss" ]; then
        # Active XSS testing via dalfox (separate code path)
        RESULT=$(run_xss_scan "$URL" "$SCAN_BUDGET")
    elif [ "$MODE" = "tls" ]; then
        # TLS/SSL configuration audit via testssl.sh (separate code path)
        RESULT=$(run_tls_scan "$URL" "$SCAN_BUDGET")
    elif [ "$MODE" = "deps" ]; then
        # Dependency/secret/IaC scan of a GIT REPO url via trivy (separate path)
        RESULT=$(run_deps_scan "$URL" "$SCAN_BUDGET")
    else
        # Signature-based scanning via nuclei. Hard wall-clock budget so a
        # throttling/slow target cannot hang the single-scan queue.
        RESULT=$(timeout -k 30 "$SCAN_BUDGET" nuclei -u "$URL" $TARGS \
            "${NUCLEI_AUTH[@]}" \
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
