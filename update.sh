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

# 2. Ensure backend symlink is in place (replaces old cp -r approach)
info "Checking backend symlink..."
if [[ -L "$PLATFORM_DIR/app/backend" ]]; then
    log "Backend symlink already in place"
elif [[ -d "$PLATFORM_DIR/app/backend" ]]; then
    warn "$PLATFORM_DIR/app/backend is a directory (old install). Replacing with symlink..."
    rm -rf "$PLATFORM_DIR/app/backend"
    ln -s "$REPO_DIR/backend" "$PLATFORM_DIR/app/backend"
    log "Backend symlink created"
else
    ln -s "$REPO_DIR/backend" "$PLATFORM_DIR/app/backend"
    log "Backend symlink created"
fi

# 3. Rebuild backend
info "Rebuilding backend Docker image..."
cd "$PLATFORM_DIR"
docker compose up -d --build backend
log "Backend rebuilt and restarted"

# 3b. Add GITHUB_TOKEN to .env if not present
if ! grep -q "^GITHUB_TOKEN=" "$PLATFORM_DIR/.env"; then
    warn "GITHUB_TOKEN belum ada di .env"
    warn "Tambahkan manual: echo 'GITHUB_TOKEN=ghp_xxx' >> $PLATFORM_DIR/.env"
    warn "Lalu: docker compose restart backend"
fi

# 4. Regenerate management script
info "Updating management script..."
source "$REPO_DIR/install.sh"
write_manage_script
log "Management script updated"

# 4b. Reconfigure OpenClaw (bind=lan, /v1/responses, token sync)
info "Reconfiguring OpenClaw..."
configure_openclaw

# 4c. Update OPENCLAW_BASE_URL to correct Docker gateway
if docker network inspect ai-platform_default &>/dev/null; then
    DOCKER_GW=$(docker network inspect ai-platform_default --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}' 2>/dev/null)
    if [[ -n "$DOCKER_GW" ]]; then
        sed -i "s|OPENCLAW_BASE_URL=.*|OPENCLAW_BASE_URL=http://$DOCKER_GW:18789|" "$PLATFORM_DIR/.env"
        log "OPENCLAW_BASE_URL updated to http://$DOCKER_GW:18789"
        docker compose -f "$PLATFORM_DIR/docker-compose.yml" restart backend 2>/dev/null || true
    fi
fi

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
