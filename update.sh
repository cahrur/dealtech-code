#!/bin/bash
# AI Platform — Update Script
# Usage: sudo bash update.sh
# Jalankan di VPS setelah git pull untuk update backend + management script

set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
PLATFORM_DIR="/srv/ai-platform"

GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'
log()  { echo -e "${GREEN}[✓]${NC} $1"; }
info() { echo -e "${CYAN}[i]${NC} $1"; }

echo -e "${BOLD}${CYAN}"
echo "╔══════════════════════════════════════╗"
echo "║     AI Platform — Update             ║"
echo "╚══════════════════════════════════════╝"
echo -e "${NC}"

# 1. Pull latest code
info "Pulling latest code from GitHub..."
git -C "$REPO_DIR" pull
log "Code updated"

# 2. Copy backend source
info "Copying backend source..."
cp -r "$REPO_DIR/backend/"* "$PLATFORM_DIR/app/backend/"
log "Backend source copied"

# 3. Rebuild backend
info "Rebuilding backend Docker image..."
cd "$PLATFORM_DIR"
docker compose up -d --build backend
log "Backend rebuilt and restarted"

# 4. Regenerate management script
info "Updating management script..."
source "$REPO_DIR/install.sh"
write_manage_script
log "Management script updated"

# 5. Update Caddyfile — tambah 9router proxy jika belum ada
info "Checking Caddyfile for 9router proxy..."
if ! grep -q "9router" "$PLATFORM_DIR/Caddyfile"; then
    python3 -c "
content = open('$PLATFORM_DIR/Caddyfile').read()
block = '''
  handle_path /9router/* {
    reverse_proxy localhost:20128
  }
'''
content = content.replace('  log {', block + '  log {')
open('$PLATFORM_DIR/Caddyfile', 'w').write(content)
"
    cd "$PLATFORM_DIR" && docker compose exec caddy caddy reload --config /etc/caddy/Caddyfile 2>/dev/null \
        || docker compose restart caddy
    log "Caddyfile updated — 9router proxy added"
else
    log "Caddyfile sudah ada 9router proxy"
fi

echo ""
echo -e "${BOLD}Update selesai!${NC}"
echo ""
echo "Cek status:"
echo "  docker compose ps"
echo "  docker compose logs -f backend"
