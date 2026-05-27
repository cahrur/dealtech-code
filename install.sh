#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════════════
# AI Coding Agent Platform — Linux Installer
# Stack: Rust Backend + OpenClaw Gateway + Postgres + Redis + Caddy
# Supported: Ubuntu 22.04+ / Debian 12+
# Usage: sudo bash install.sh
# ═══════════════════════════════════════════════════════════════════════════════

set -euo pipefail

# ─── Colors ───────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
BLUE='\033[0;34m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

# ─── Platform directories ─────────────────────────────────────────────────────
PLATFORM_DIR="/srv/ai-platform"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$PLATFORM_DIR/app"
DATA_DIR="$PLATFORM_DIR/data"
WORKSPACES_DIR="$PLATFORM_DIR/workspaces"
WORKTREES_DIR="$PLATFORM_DIR/worktrees"
LOGS_DIR="$PLATFORM_DIR/logs"
BACKUPS_DIR="$PLATFORM_DIR/backups"
COMPOSE_FILE="$PLATFORM_DIR/docker-compose.yml"
ENV_FILE="$PLATFORM_DIR/.env"
CADDYFILE="$PLATFORM_DIR/Caddyfile"
OPENCLAW_PORT=18789

# ─── Runtime vars (filled by collect_config) ──────────────────────────────────
DOMAIN=""; ADMIN_EMAIL=""; PG_PASS=""; OPENCLAW_TOKEN=""; ADMIN_API_KEY=""
ANTHROPIC_API_KEY=""; OPENAI_API_KEY=""; GITHUB_TOKEN=""

# ─── Helpers ──────────────────────────────────────────────────────────────────
log()     { echo -e "${GREEN}[✓]${NC} $1"; }
warn()    { echo -e "${YELLOW}[!]${NC} $1"; }
error()   { echo -e "${RED}[✗]${NC} $1"; exit 1; }
info()    { echo -e "${BLUE}[i]${NC} $1"; }
section() { echo -e "\n${BOLD}${CYAN}══ $1 ══${NC}"; }
gen_secret() { openssl rand -hex 32; }
gen_token()  { openssl rand -base64 48 | tr -d '\n/+=' | head -c 64; }

# ─── Root check ───────────────────────────────────────────────────────────────
check_root() {
  [[ $EUID -eq 0 ]] || error "Run as root: sudo bash install.sh"
}

# ─── OS check ─────────────────────────────────────────────────────────────────
check_os() {
  section "Checking OS"
  if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    info "OS: $PRETTY_NAME"
    case "$ID" in
      ubuntu|debian) log "Supported OS detected" ;;
      *) warn "Untested OS: $ID — proceeding anyway" ;;
    esac
  else
    warn "Cannot detect OS — proceeding anyway"
  fi
  ARCH=$(uname -m)
  info "Architecture: $ARCH"
  [[ "$ARCH" == "x86_64" || "$ARCH" == "aarch64" ]] || warn "Untested architecture: $ARCH"
}

# ─── Collect config ───────────────────────────────────────────────────────────
collect_config() {
  section "Configuration"
  echo -e "${YELLOW}Press Enter to accept auto-generated values${NC}\n"

  read -rp "Domain (e.g. ai.company.com): " DOMAIN
  [[ -n "$DOMAIN" ]] || error "Domain is required"

  read -rp "Admin email (for TLS cert): " ADMIN_EMAIL
  [[ -n "$ADMIN_EMAIL" ]] || error "Admin email is required"

  read -rp "Postgres password [auto-generate]: " PG_PASS
  PG_PASS="${PG_PASS:-$(gen_secret)}"

  read -rp "OpenClaw Gateway token [auto-generate]: " OPENCLAW_TOKEN
  OPENCLAW_TOKEN="${OPENCLAW_TOKEN:-$(gen_token)}"

  read -rp "Admin API key [auto-generate]: " ADMIN_API_KEY
  ADMIN_API_KEY="${ADMIN_API_KEY:-ak_$(gen_token)}"

  echo ""
  read -rp "Anthropic API key (Claude models, optional — Enter to skip): " ANTHROPIC_API_KEY
  read -rp "OpenAI API key (GPT models, optional — Enter to skip): " OPENAI_API_KEY
  read -rp "GitHub Personal Access Token (untuk push ke repo, optional): " GITHUB_TOKEN

  echo ""
  read -rp "Telegram Bot Token (dari @BotFather, optional — Enter to skip): " TELEGRAM_BOT_TOKEN
  if [[ -n "$TELEGRAM_BOT_TOKEN" ]]; then
    TELEGRAM_ENABLED=true
    info "Telegram bot akan diaktifkan"
  else
    TELEGRAM_ENABLED=false
    info "Telegram bot dinonaktifkan (bisa diaktifkan nanti via .env)"
  fi

  echo ""
  info "Domain:         $DOMAIN"
  info "Admin email:    $ADMIN_EMAIL"
  info "Postgres pass:  ${PG_PASS:0:8}... (truncated)"
  info "OpenClaw token: ${OPENCLAW_TOKEN:0:8}... (truncated)"
  info "Admin API key:  ${ADMIN_API_KEY:0:12}... (truncated)"
  echo ""

  read -rp "Proceed? [Y/n]: " CONFIRM
  CONFIRM="${CONFIRM:-Y}"
  [[ "$CONFIRM" =~ ^[Yy]$ ]] || error "Installation cancelled"
}

# ─── Install system dependencies ──────────────────────────────────────────────
install_deps() {
  section "Installing system dependencies"
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq \
    curl wget git openssl ufw \
    ca-certificates gnupg lsb-release \
    apt-transport-https software-properties-common \
    2>/dev/null
  log "Base packages installed"

  if ! command -v docker &>/dev/null; then
    info "Installing Docker..."
    curl -fsSL https://get.docker.com | sh
    systemctl enable --now docker
    log "Docker installed: $(docker --version)"
  else
    log "Docker already present: $(docker --version)"
  fi

  if ! docker compose version &>/dev/null 2>&1; then
    info "Installing Docker Compose plugin..."
    apt-get install -y -qq docker-compose-plugin
    log "Docker Compose installed"
  else
    log "Docker Compose: $(docker compose version --short 2>/dev/null || echo 'present')"
  fi
}

# ─── Create directory structure ───────────────────────────────────────────────
create_dirs() {
  section "Creating directory structure"
  local dirs=(
    "$DATA_DIR/postgres/init"
    "$DATA_DIR/redis"
    "$WORKSPACES_DIR"
    "$WORKTREES_DIR"
    "$LOGS_DIR/backend"
    "$LOGS_DIR/agent-runs"
    "$LOGS_DIR/caddy"
    "$BACKUPS_DIR"
  )
  for d in "${dirs[@]}"; do mkdir -p "$d"; done

  chmod 750 "$PLATFORM_DIR"
  chmod 700 "$DATA_DIR"
  chmod 755 "$WORKSPACES_DIR" "$WORKTREES_DIR"

  log "Directory structure created at $PLATFORM_DIR"
  info "  $APP_DIR/backend    — Rust backend source"
  info "  $WORKSPACES_DIR     — canonical repo clones"
  info "  $WORKTREES_DIR      — disposable run worktrees"
  info "  $LOGS_DIR           — backend + agent run logs"
  info "  $BACKUPS_DIR        — database backups"
}

# ─── Write .env ───────────────────────────────────────────────────────────────
write_env() {
  section "Writing .env"
  cat > "$ENV_FILE" <<EOF
# AI Coding Agent Platform — Environment Config
# Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)
# KEEP THIS FILE SECRET — chmod 600 is enforced

# ── Domain ────────────────────────────────────────────────────────────────────
DOMAIN=${DOMAIN}
ADMIN_EMAIL=${ADMIN_EMAIL}

# ── Database (key-value format, not URL) ──────────────────────────────────────
DB_HOST=postgres
DB_PORT=5432
DB_NAME=aicode
DB_USER=postgres
DB_PASSWORD=${PG_PASS}

# ── Redis ─────────────────────────────────────────────────────────────────────
REDIS_HOST=redis
REDIS_PORT=6379

# ── API Key Auth ──────────────────────────────────────────────────────────────
ADMIN_API_KEY=${ADMIN_API_KEY}

# ── 9router (AI provider management) ─────────────────────────────────────────
ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
OPENAI_API_KEY=${OPENAI_API_KEY}
NINEROUTER_PORT=4000

# ── GitHub ────────────────────────────────────────────────────────────────────
GITHUB_TOKEN=${GITHUB_TOKEN}

# ── OpenClaw Gateway ──────────────────────────────────────────────────────────
OPENCLAW_BASE_URL=http://host.docker.internal:${OPENCLAW_PORT}
OPENCLAW_GATEWAY_TOKEN=${OPENCLAW_TOKEN}

# ── Telegram Bot ─────────────────────────────────────────────────────────────
TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN:-}
TELEGRAM_ENABLED=${TELEGRAM_ENABLED:-false}

# ── Security ─────────────────────────────────────────────────────────────────
# CORS_ORIGIN: comma-separated allowed origins. "*" = allow all (dev only)
# Production example: CORS_ORIGIN=https://app.dealtech.ai,https://admin.dealtech.ai
CORS_ORIGIN=${CORS_ORIGIN:-*}

# ── App ───────────────────────────────────────────────────────────────────────
APP_ENV=production
APP_PORT=8080
LOG_LEVEL=info

# ── Performance & Limits ──────────────────────────────────────────────────────
# MAX_CONCURRENT_RUNS: max run bersamaan. Rekomendasi: 3 untuk 4GB RAM, 5 untuk 8GB+
MAX_CONCURRENT_RUNS=${MAX_CONCURRENT_RUNS:-3}
USER_RATE_LIMIT_PER_MINUTE=${USER_RATE_LIMIT_PER_MINUTE:-10}
GRACEFUL_SHUTDOWN=true
OPENCLAW_MAX_RETRIES=3

# ── Platform paths (mounted into container) ───────────────────────────────────
WORKSPACES_PATH=/srv/ai-platform/workspaces
WORKTREES_PATH=/srv/ai-platform/worktrees
LOGS_PATH=/srv/ai-platform/logs
EOF
  chmod 600 "$ENV_FILE"
  log ".env written (chmod 600)"
}

# ─── Write docker-compose.yml ─────────────────────────────────────────────────
write_compose() {
  section "Writing docker-compose.yml"
  cat > "$COMPOSE_FILE" <<'COMPOSE'
services:
  caddy:
    image: caddy:2-alpine
    restart: unless-stopped
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "5"
    ports:
      - "80:80"
      - "443:443"
      - "443:443/udp"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
      - caddy_config:/config
    depends_on:
      backend:
        condition: service_healthy

  backend:
    build:
      context: ./app/backend
      dockerfile: Dockerfile
    restart: unless-stopped
    logging:
      driver: "json-file"
      options:
        max-size: "50m"
        max-file: "10"
    env_file: .env
    ports:
      - "127.0.0.1:8080:8080"
    extra_hosts:
      - "host.docker.internal:host-gateway"
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock
      - /srv/ai-platform/workspaces:/srv/ai-platform/workspaces
      - /srv/ai-platform/worktrees:/srv/ai-platform/worktrees
      - /srv/ai-platform/logs:/srv/ai-platform/logs
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_healthy
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 30s
      timeout: 10s
      retries: 3
      start_period: 40s

  postgres:
    image: postgres:16-alpine
    restart: unless-stopped
    logging:
      driver: "json-file"
      options:
        max-size: "20m"
        max-file: "5"
    environment:
      POSTGRES_DB: ${DB_NAME}
      POSTGRES_USER: ${DB_USER}
      POSTGRES_PASSWORD: ${DB_PASSWORD}
    volumes:
      - postgres_data:/var/lib/postgresql/data
      - ./data/postgres/init:/docker-entrypoint-initdb.d:ro
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${DB_USER} -d ${DB_NAME}"]
      interval: 10s
      timeout: 5s
      retries: 5

  redis:
    image: redis:7-alpine
    restart: unless-stopped
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"
    command: >
      redis-server
      --appendonly yes
      --maxmemory 512mb
      --maxmemory-policy allkeys-lru
    volumes:
      - redis_data:/data
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 10s
      timeout: 5s
      retries: 5

volumes:
  postgres_data:
  redis_data:
  caddy_data:
  caddy_config:
COMPOSE
  log "docker-compose.yml written"
}

# ─── Write Caddyfile ──────────────────────────────────────────────────────────
write_caddyfile() {
  section "Writing Caddyfile"
  cat > "$CADDYFILE" <<EOF
{
  email ${ADMIN_EMAIL}
}

${DOMAIN} {
  header {
    Strict-Transport-Security "max-age=31536000; includeSubDomains; preload"
    X-Content-Type-Options "nosniff"
    X-Frame-Options "DENY"
    X-XSS-Protection "1; mode=block"
    Referrer-Policy "strict-origin-when-cross-origin"
    -Server
  }

  @ws {
    header Connection *Upgrade*
    header Upgrade websocket
  }
  reverse_proxy @ws backend:8080

  reverse_proxy /api/* backend:8080
  reverse_proxy /health backend:8080

  handle_path /9router/* {
    reverse_proxy localhost:20128
  }

  log {
    output file /var/log/caddy/access.log
    format json
  }
}
EOF
  log "Caddyfile written"
}

# ─── Firewall ─────────────────────────────────────────────────────────────────
setup_firewall() {
  section "Configuring UFW firewall"
  ufw --force reset
  ufw default deny incoming
  ufw default allow outgoing
  ufw allow 22/tcp  comment "SSH"
  ufw allow 80/tcp  comment "HTTP"
  ufw allow 443/tcp comment "HTTPS"
  ufw allow from 172.16.0.0/12 to any port 18789 comment "Docker to OpenClaw"
  ufw --force enable
  log "Firewall configured (22, 80, 443 open)"
  warn "OpenClaw port $OPENCLAW_PORT is NOT exposed — private only"
}

# ─── Backend Dockerfile placeholder ──────────────────────────────────────────
write_backend_placeholder() {
  # Skip placeholder if backend is already a symlink or has real source
  if [[ -L "$APP_DIR/backend" || -f "$APP_DIR/backend/Cargo.toml" ]]; then
    info "Backend source already present — skipping placeholder"
    return
  fi
  section "Writing backend Dockerfile placeholder"
  cat > "$dockerfile" <<'EOF'
# Build stage
FROM rust:1.78-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY . .
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/backend .
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=10s --retries=3 \
  CMD curl -f http://localhost:8080/health || exit 1
CMD ["./backend"]
EOF
  log "Backend Dockerfile placeholder written"
  warn "Replace with your actual Rust backend source before 'docker compose up --build'"
}

# ─── Systemd service ──────────────────────────────────────────────────────────
write_systemd() {
  section "Writing systemd service"
  cat > /etc/systemd/system/ai-platform.service <<EOF
[Unit]
Description=AI Coding Agent Platform
Requires=docker.service
After=docker.service network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
WorkingDirectory=${PLATFORM_DIR}
ExecStart=/usr/bin/docker compose up -d --remove-orphans
ExecStop=/usr/bin/docker compose down
TimeoutStartSec=300

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable ai-platform.service
  log "Systemd service enabled (ai-platform.service)"
}

# ─── Daily Postgres backup cron ───────────────────────────────────────────────
write_backup_cron() {
  section "Setting up daily Postgres backup"
  cat > /etc/cron.daily/ai-platform-backup <<EOF
#!/bin/bash
set -euo pipefail
BACKUP_DIR="${BACKUPS_DIR}"
DATE=\$(date +%Y%m%d_%H%M%S)
FILE="\${BACKUP_DIR}/postgres_\${DATE}.sql.gz"
cd "${PLATFORM_DIR}"
docker compose exec -T postgres pg_dump -U postgres aicode | gzip > "\${FILE}"
find "\${BACKUP_DIR}" -name "postgres_*.sql.gz" -mtime +7 -delete
echo "Backup done: \${FILE}"
EOF
  chmod +x /etc/cron.daily/ai-platform-backup
  log "Daily Postgres backup cron written (/etc/cron.daily/ai-platform-backup)"
}

# ─── Start infra services ─────────────────────────────────────────────────────
start_services() {
  section "Starting infrastructure services"
  cd "$PLATFORM_DIR"

  info "Pulling Docker images..."
  docker compose pull --quiet postgres redis 2>/dev/null || true

  info "Starting Postgres and Redis..."
  docker compose up -d postgres redis

  info "Waiting for Postgres..."
  local retries=0
  until docker compose exec -T postgres pg_isready -U postgres -d aicode &>/dev/null; do
    ((retries++))
    [[ $retries -gt 30 ]] && error "Postgres failed to start after 60s"
    sleep 2
  done
  log "Postgres ready"

  info "Waiting for Redis..."
  retries=0
  until docker compose exec -T redis redis-cli ping &>/dev/null; do
    ((retries++))
    [[ $retries -gt 15 ]] && error "Redis failed to start after 30s"
    sleep 2
  done
  log "Redis ready"

  # Detect Docker gateway IP and update OPENCLAW_BASE_URL
  if docker network inspect ai-platform_default &>/dev/null; then
    DOCKER_GW=$(docker network inspect ai-platform_default --format '{{range .IPAM.Config}}{{.Gateway}}{{end}}' 2>/dev/null)
    if [[ -n "$DOCKER_GW" ]]; then
      sed -i "s|OPENCLAW_BASE_URL=.*|OPENCLAW_BASE_URL=http://$DOCKER_GW:18789|" "$ENV_FILE"
      log "OPENCLAW_BASE_URL updated to http://$DOCKER_GW:18789"
    fi
  fi

  log "Infrastructure services running"

  # Link backend source from repo (symlink so no manual sync needed)
  if [[ -d "$REPO_DIR/backend" ]]; then
    if [[ -L "$APP_DIR/backend" ]]; then
      info "Backend symlink already exists — skipping"
    elif [[ -d "$APP_DIR/backend" ]]; then
      warn "$APP_DIR/backend is a directory, replacing with symlink..."
      rm -rf "$APP_DIR/backend"
      ln -s "$REPO_DIR/backend" "$APP_DIR/backend"
      log "Backend symlink created: $APP_DIR/backend -> $REPO_DIR/backend"
    else
      ln -s "$REPO_DIR/backend" "$APP_DIR/backend"
      log "Backend symlink created: $APP_DIR/backend -> $REPO_DIR/backend"
    fi
  else
    warn "Backend source not found at $REPO_DIR/backend — creating $APP_DIR/backend as directory"
    mkdir -p "$APP_DIR/backend"
  fi

  # Build and start backend
  if [[ -f "$APP_DIR/backend/Cargo.toml" ]]; then
    info "Building backend Docker image..."
    cd "$PLATFORM_DIR"
    docker compose build backend 2>/dev/null && log "Backend built" || warn "Backend build failed"
    docker compose up -d
    log "All services started"
  else
    warn "Backend source not found at $APP_DIR/backend/ — start manually after copying source"
  fi
}

# ─── Install Node.js and 9router ─────────────────────────────────────────────
install_nodejs_9router() {
  section "Installing Node.js and 9router"
  if ! command -v node &>/dev/null; then
    info "Installing Node.js 20..."
    curl -fsSL https://deb.nodesource.com/setup_22.x | bash - 2>/dev/null
    apt-get install -y -qq nodejs
    log "Node.js installed: $(node --version)"
  else
    log "Node.js already present: $(node --version)"
  fi
  npm install -g 9router --quiet 2>/dev/null || warn "9router install failed — install manually: npm install -g 9router"
  command -v 9router &>/dev/null && log "9router installed" || warn "9router not found in PATH"
}

# ─── Install OpenClaw Gateway ─────────────────────────────────────────────────
install_openclaw() {
  section "Installing OpenClaw Gateway"
  if command -v openclaw &>/dev/null; then
    log "OpenClaw already installed"
    return
  fi
  info "Installing OpenClaw via npm..."
  npm install -g openclaw@latest --quiet 2>/dev/null
  if command -v openclaw &>/dev/null; then
    log "OpenClaw installed"
    warn "Jalankan setup awal: openclaw onboard --install-daemon"
  else
    warn "OpenClaw install gagal — install manual: npm install -g openclaw@latest"
  fi
}

# ─── Configure OpenClaw Gateway ──────────────────────────────────────────────
configure_openclaw() {
  section "Configuring OpenClaw Gateway"
  local config_file="$HOME/.openclaw/openclaw.json"
  if [[ ! -f "$config_file" ]]; then
    warn "OpenClaw config not found — jalankan 'openclaw onboard --install-daemon' dulu"
    return
  fi
  info "Setting bind=lan, enabling /v1/responses, syncing token..."
  echo "import json,os,shutil" > /tmp/oc_cfg.py
  echo "f=os.path.expanduser('~/.openclaw/openclaw.json')" >> /tmp/oc_cfg.py
  echo "c=json.load(open(f))" >> /tmp/oc_cfg.py
  echo "shutil.copy(f,f+'.bak')" >> /tmp/oc_cfg.py
  echo "c.setdefault('gateway',{})['bind']='lan'" >> /tmp/oc_cfg.py
  echo "c.setdefault('gateway',{}).setdefault('http',{}).setdefault('endpoints',{}).setdefault('responses',{})['enabled']=True" >> /tmp/oc_cfg.py
  echo "e='/srv/ai-platform/.env'" >> /tmp/oc_cfg.py
  echo "t=[l.strip().split('=',1)[1] for l in open(e) if l.startswith('OPENCLAW_GATEWAY_TOKEN=')] if os.path.exists(e) else []" >> /tmp/oc_cfg.py
  echo "if t: c.setdefault('gateway',{}).setdefault('auth',{})['token']=t[0]" >> /tmp/oc_cfg.py
  echo "json.dump(c,open(f,'w'),indent=2)" >> /tmp/oc_cfg.py
  python3 /tmp/oc_cfg.py && log "OpenClaw configured (bind=lan, /v1/responses=enabled, token synced)" || warn "OpenClaw config failed"
  rm -f /tmp/oc_cfg.py
  pgrep -f openclaw > /dev/null && { pkill -f openclaw 2>/dev/null; sleep 2; nohup openclaw start > /srv/ai-platform/logs/openclaw.log 2>&1 & log "OpenClaw restarted"; }
}

# ─── Install Hermes Agent ─────────────────────────────────────────────────────
install_hermes() {
  section "Installing Hermes Agent"
  if command -v hermes &>/dev/null; then
    log "Hermes already installed"
    return
  fi
  info "Installing Hermes via npm..."
  npm install -g @hermes-ai/agent@latest --quiet 2>/dev/null || \
  npm install -g hermes-agent@latest --quiet 2>/dev/null
  if command -v hermes &>/dev/null; then
    log "Hermes installed"
    warn "Jalankan setup awal: hermes onboard"
  else
    warn "Hermes tidak tersedia via npm"
    info "Konfigurasi model hermes-3 di 9router untuk menggunakan Hermes"
  fi
}

# ─── Write 9router config ─────────────────────────────────────────────────────
write_9router_config() {
  section "Writing 9router config"
  mkdir -p "$PLATFORM_DIR/9router"
  cat > "$PLATFORM_DIR/9router/config.json" <<EOF
{
  "port": 4000,
  "providers": {
    "anthropic": {
      "apiKey": "${ANTHROPIC_API_KEY:-REPLACE_WITH_ANTHROPIC_KEY}",
      "models": ["claude-opus-4-7", "claude-sonnet-4-6", "claude-haiku-4-5"]
    },
    "openai": {
      "apiKey": "${OPENAI_API_KEY:-REPLACE_WITH_OPENAI_KEY}",
      "models": ["gpt-4o", "gpt-4o-mini"]
    }
  },
  "defaultModel": "claude-sonnet-4-6",
  "logging": true
}
EOF
  chmod 600 "$PLATFORM_DIR/9router/config.json"
  log "9router config written (chmod 600)"
}

# ─── Write management script ──────────────────────────────────────────────────
write_manage_script() {
  section "Writing management script"
  cat > "$PLATFORM_DIR/manage.sh" <<'MANAGE'
#!/bin/bash
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'
PLATFORM_DIR="/srv/ai-platform"
log()  { echo -e "${GREEN}[✓]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
info() { echo -e "${CYAN}[i]${NC} $1"; }

admin_menu() {
  ADMIN_KEY=$(grep "^ADMIN_API_KEY=" "$PLATFORM_DIR/.env" 2>/dev/null | cut -d= -f2)
  DOMAIN=$(grep "^DOMAIN=" "$PLATFORM_DIR/.env" 2>/dev/null | cut -d= -f2)
  BASE_URL="http://localhost:8080"
  while true; do
    echo ""
    echo -e "${BOLD}${CYAN}╔══════════════════════════════════════╗${NC}"
    echo -e "${BOLD}${CYAN}║         Admin Panel                  ║${NC}"
    echo -e "${BOLD}${CYAN}╚══════════════════════════════════════╝${NC}"
    echo "  1) List semua API key"
    echo "  2) Buat API key baru"
    echo "  3) Revoke API key"
    echo "  4) Usage stats"
    echo "  0) Kembali ke menu utama"
    echo ""
    read -rp "Choose [0-4]: " ch
    case "$ch" in
      1)
        echo "" && curl -s -H "X-API-Key: $ADMIN_KEY" "$BASE_URL/api/apikeys" | python3 -m json.tool
        ;;
      2)
        read -rp "Nama key: " name
        read -rp "Role (admin/developer/viewer): " role
        echo "" && curl -s -X POST \
          -H "X-API-Key: $ADMIN_KEY" \
          -H "Content-Type: application/json" \
          -d "{\"name\":\"$name\",\"role\":\"$role\"}" \
          "$BASE_URL/api/apikeys" | python3 -m json.tool
        echo -e "\n${YELLOW}[!]${NC} Simpan key di atas — hanya tampil sekali!"
        ;;
      3)
        read -rp "Key ID (UUID): " kid
        echo "" && curl -s -X POST \
          -H "X-API-Key: $ADMIN_KEY" \
          "$BASE_URL/api/apikeys/$kid/revoke" | python3 -m json.tool
        ;;
      4)
        echo "" && curl -s -H "X-API-Key: $ADMIN_KEY" "$BASE_URL/api/usage" | python3 -m json.tool
        ;;
      0) break ;;
      *) warn "Invalid option" ;;
    esac
  done
}

show_menu() {
  while true; do
    echo ""
    echo -e "${BOLD}${CYAN}╔══════════════════════════════════════╗${NC}"
    echo -e "${BOLD}${CYAN}║     AI Platform Management Menu      ║${NC}"
    echo -e "${BOLD}${CYAN}╚══════════════════════════════════════╝${NC}"
    echo "  1) Start 9router"
    echo "  2) Stop 9router"
    echo "  3) Status 9router"
    echo "  4) Open 9router (Web UI URL)"
    echo "  5) Start platform (docker compose up)"
    echo "  6) Stop platform (docker compose down)"
    echo "  7) Restart backend"
    echo "  8) View backend logs"
    echo "  9) View all logs"
    echo "  b) OpenClaw — onboard (setup awal)"
    echo "  c) OpenClaw — status"
    echo "  d) Hermes — onboard (setup awal)"
    echo "  e) Hermes — status"
    echo "  a) Admin Panel"
    echo "  0) Exit"
    echo ""
    read -rp "Choose [0-9]: " choice
    case "$choice" in
      1)
        mkdir -p "$PLATFORM_DIR/logs"
        nohup 9router start --config "$PLATFORM_DIR/9router/config.json" \
          > "$PLATFORM_DIR/logs/9router.log" 2>&1 &
        log "9router started on port 4000"
        ;;
      2) pkill -f "9router" 2>/dev/null && log "9router stopped" || warn "9router was not running" ;;
      3) pgrep -f "9router" > /dev/null && log "9router is running" || warn "9router is NOT running" ;;
      4)
        DOMAIN=$(grep "^DOMAIN=" "$PLATFORM_DIR/.env" 2>/dev/null | cut -d= -f2)
        info "9router Web UI: https://$DOMAIN/9router"
        ;;
      5) cd "$PLATFORM_DIR" && docker compose up -d && log "Platform started" ;;
      6) cd "$PLATFORM_DIR" && docker compose down && log "Platform stopped" ;;
      7) cd "$PLATFORM_DIR" && docker compose restart backend && log "Backend restarted" ;;
      8) cd "$PLATFORM_DIR" && docker compose logs -f backend ;;
      9) cd "$PLATFORM_DIR" && docker compose logs -f ;;
      b|B) openclaw onboard --install-daemon ;;
      c|C)
        pgrep -f openclaw > /dev/null && log "OpenClaw running" || warn "OpenClaw not running"
        ss -tlnp | grep 18789 && log "Port 18789 listening" || warn "Port 18789 not listening"
        ;;
      d|D) hermes onboard 2>/dev/null || warn "Hermes tidak terinstall — konfigurasi model hermes-3 di 9router" ;;
      e|E)
        command -v hermes > /dev/null && log "Hermes installed" || warn "Hermes not installed"
        pgrep -f hermes > /dev/null && log "Hermes running" || info "Hermes menggunakan 9router sebagai backend"
        ;;
      a|A) admin_menu ;;
      0) break ;;
      *) warn "Invalid option" ;;
    esac
  done
}

case "${1:-menu}" in
  menu)   show_menu ;;
  start)  nohup 9router start --config "$PLATFORM_DIR/9router/config.json" > "$PLATFORM_DIR/logs/9router.log" 2>&1 & log "9router started" ;;
  stop)   pkill -f "9router" && log "9router stopped" || warn "Not running" ;;
  status) pgrep -f "9router" > /dev/null && log "running" || warn "stopped" ;;
  logs)   cd "$PLATFORM_DIR" && docker compose logs -f backend ;;
  *)      echo "Usage: $0 [menu|start|stop|status|logs]" ;;
esac
MANAGE
  chmod +x "$PLATFORM_DIR/manage.sh"
  log "Management script: $PLATFORM_DIR/manage.sh"
}
print_summary() {
  echo ""
  echo -e "${BOLD}${GREEN}╔══════════════════════════════════════════════════╗${NC}"
  echo -e "${BOLD}${GREEN}║         Installation Complete                    ║${NC}"
  echo -e "${BOLD}${GREEN}╚══════════════════════════════════════════════════╝${NC}"
  echo ""
  echo -e "${BOLD}Platform:${NC}  $PLATFORM_DIR"
  echo -e "${BOLD}Domain:${NC}    https://$DOMAIN"
  echo -e "${BOLD}Config:${NC}    $ENV_FILE"
  echo -e "${BOLD}Compose:${NC}   $COMPOSE_FILE"
  echo ""
  echo -e "${BOLD}${CYAN}Next steps:${NC}"
  echo ""
  echo "  1. Install OpenClaw Gateway (bind to 127.0.0.1:$OPENCLAW_PORT)"
  echo "     Docs: https://docs.openclaw.ai/install/docker"
  echo ""
  echo "  2. Add AI provider keys to 9router config:"
  echo "     $PLATFORM_DIR/9router/config.json"
  echo ""
  echo "  3. Start 9router:"
  echo "     bash $PLATFORM_DIR/manage.sh start"
  echo ""
  echo "  4. Copy Rust backend source (if not already built):"
  echo "     scp -r backend/ user@vps:$APP_DIR/backend/"
  echo "     cd $PLATFORM_DIR && docker compose up -d --build"
  echo ""
  echo "  5. Setup Telegram Bot (opsional):"
  echo "     a. Buat bot di @BotFather, dapat token"
  echo "     b. Edit .env: TELEGRAM_BOT_TOKEN=<token> dan TELEGRAM_ENABLED=true"
  echo "     c. Restart: cd $PLATFORM_DIR && docker compose up -d --no-deps backend"
  echo "     d. Daftarkan admin pertama ke whitelist:"
  echo "        docker exec ai-platform-postgres-1 psql -U postgres -d aicode -c \\""
  echo "        INSERT INTO telegram_users (telegram_id, user_id, name)"
  echo "        VALUES (<telegram_id>, '<user_id_dari_DB>', '<nama>');\\""
  echo ""
  echo "  6. Setup Swap (WAJIB untuk VPS RAM <= 4GB):"
  echo "     fallocate -l 2G /swapfile"
  echo "     chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile"
  echo "     echo '/swapfile none swap sw 0 0' >> /etc/fstab"
  echo "     # Verifikasi: free -h"
  echo ""
  echo "  6. Verify health:"
  echo "     curl https://$DOMAIN/health"
  echo ""
  echo -e "${BOLD}Management menu (anytime):${NC}"
  echo "     bash $PLATFORM_DIR/manage.sh"
  echo ""
  echo -e "${BOLD}${YELLOW}Security reminders:${NC}"
  echo "  - $ENV_FILE is chmod 600 — keep it secret"
  echo "  - OpenClaw port $OPENCLAW_PORT must NOT be exposed publicly"
  echo "  - Rotate ADMIN_API_KEY and OPENCLAW_GATEWAY_TOKEN periodically"
  echo "  - Daily backup: /etc/cron.daily/ai-platform-backup"
  echo ""
  echo -e "${BOLD}Manage services:${NC}"
  echo "  systemctl start|stop|status ai-platform"
  echo "  cd $PLATFORM_DIR && docker compose logs -f backend"
  echo ""
}

# ─── Main ─────────────────────────────────────────────────────────────────────
main() {
  echo -e "${BOLD}${CYAN}"
  echo "╔══════════════════════════════════════════════════╗"
  echo "║  AI Coding Agent Platform — Linux Installer      ║"
  echo "║  Rust + OpenClaw + Postgres + Redis + Caddy      ║"
  echo "╚══════════════════════════════════════════════════╝"
  echo -e "${NC}"

  check_root
  check_os
  collect_config
  install_deps
  install_nodejs_9router
  install_openclaw
  install_hermes
  create_dirs
  write_env
  write_compose
  write_caddyfile
  write_9router_config
  configure_openclaw
  setup_firewall
  write_backend_placeholder
  write_systemd
  write_backup_cron
  write_manage_script
  start_services
  print_summary
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi
