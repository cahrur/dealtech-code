#!/bin/bash
# Security Scanner - Main Entry Point
# Usage: secscan <url> [options]

set -e

# Resolve symlinks to get actual script directory
SOURCE="${BASH_SOURCE[0]}"
while [ -L "$SOURCE" ]; do
    SCRIPT_DIR="$(cd "$(dirname "$SOURCE")" && pwd)"
    SOURCE="$(readlink "$SOURCE")"
    [[ $SOURCE != /* ]] && SOURCE="$SCRIPT_DIR/$SOURCE"
done
SCRIPT_DIR="$(cd "$(dirname "$SOURCE")" && pwd)"
RESULTS_DIR="${SCRIPT_DIR}/results"
source "${SCRIPT_DIR}/lib/scanner.sh"
source "${SCRIPT_DIR}/lib/reporter.sh"
source "${SCRIPT_DIR}/lib/auth.sh"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Defaults
SCAN_MODE="quick"
AUTH_TOKEN=""
AUTH_COOKIE=""
HEADERS=""
OUTPUT_FORMAT="text"
SEVERITY="critical,high,medium"
TIMESTAMP=$(date +%Y%m%d_%H%M%S)

usage() {
    echo "Security Scanner - Nuclei-based security testing tool"
    echo ""
    echo "Usage: secscan <url> [options]"
    echo ""
    echo "Scan Modes:"
    echo "  --quick          Top critical checks only (default, ~2-5 min)"
    echo "  --full           All templates (~15-30 min)"
    echo "  --recon          Reconnaissance only (tech detect, info)"
    echo "  --cves           Known CVEs only"
    echo "  --misconfig      Misconfigurations only"
    echo "  --exposure       Exposed files/panels only"
    echo ""
    echo "Authentication:"
    echo "  --token <token>  Bearer/API token"
    echo "  --cookie <cookie> Session cookie (format: name=value)"
    echo "  --header <h>     Custom header (format: 'Name: Value')"
    echo ""
    echo "Output:"
    echo "  --json           JSON output"
    echo "  --markdown       Markdown report"
    echo "  --severity <s>   Filter: critical,high,medium,low,info"
    echo ""
    echo "Examples:"
    echo "  secscan https://example.com"
    echo "  secscan https://app.example.com --full --token eyJhbG..."
    echo "  secscan https://api.example.com --cves --severity critical,high"
}

# Parse arguments
if [ $# -lt 1 ]; then
    usage
    exit 1
fi

# Handle --help/-h as first arg
if [[ "$1" == "--help" || "$1" == "-h" ]]; then
    usage
    exit 0
fi

TARGET_URL="$1"
shift

# Validate URL
if [[ ! "$TARGET_URL" =~ ^https?:// ]]; then
    echo -e "${RED}❌ Invalid URL. Must start with http:// or https://${NC}"
    exit 1
fi

while [[ $# -gt 0 ]]; do
    case $1 in
        --quick) SCAN_MODE="quick"; shift ;;
        --full) SCAN_MODE="full"; shift ;;
        --recon) SCAN_MODE="recon"; shift ;;
        --cves) SCAN_MODE="cves"; shift ;;
        --misconfig) SCAN_MODE="misconfig"; shift ;;
        --exposure) SCAN_MODE="exposure"; shift ;;
        --token) AUTH_TOKEN="$2"; shift 2 ;;
        --cookie) AUTH_COOKIE="$2"; shift 2 ;;
        --header) HEADERS="${HEADERS}${2}\n"; shift 2 ;;
        --json) OUTPUT_FORMAT="json"; shift ;;
        --markdown) OUTPUT_FORMAT="markdown"; shift ;;
        --severity) SEVERITY="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo -e "${RED}Unknown option: $1${NC}"; usage; exit 1 ;;
    esac
done

# Setup output file
SCAN_ID="${TIMESTAMP}_$(echo "$TARGET_URL" | sed 's|https\?://||;s|/|_|g;s|[^a-zA-Z0-9_.-]||g')"
OUTPUT_FILE="${RESULTS_DIR}/${SCAN_ID}"

echo -e "${BLUE}🔒 Security Scanner${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "Target:   ${GREEN}${TARGET_URL}${NC}"
echo -e "Mode:     ${YELLOW}${SCAN_MODE}${NC}"
echo -e "Severity: ${SEVERITY}"
[ -n "$AUTH_TOKEN" ] && echo -e "Auth:     Token provided ✓"
[ -n "$AUTH_COOKIE" ] && echo -e "Auth:     Cookie provided ✓"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Build auth headers
AUTH_ARGS=$(build_auth_args "$AUTH_TOKEN" "$AUTH_COOKIE" "$HEADERS")

# Run scan
echo -e "${BLUE}⏳ Starting scan...${NC}"
echo ""

run_scan "$TARGET_URL" "$SCAN_MODE" "$SEVERITY" "$AUTH_ARGS" "$OUTPUT_FILE"
SCAN_EXIT=$?

echo ""

# Generate report
if [ $SCAN_EXIT -eq 0 ]; then
    generate_report "$OUTPUT_FILE" "$OUTPUT_FORMAT" "$TARGET_URL" "$SCAN_MODE"
else
    echo -e "${RED}❌ Scan failed with exit code: ${SCAN_EXIT}${NC}"
    exit $SCAN_EXIT
fi
