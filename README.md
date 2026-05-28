# Dealtech Code — AI Coding Agent Platform

Platform AI coding agent yang memungkinkan tim developer berkolaborasi dengan AI untuk mengerjakan proyek coding langsung di repository GitHub. Kirim prompt dari Android app atau Telegram, agent AI bekerja otomatis — edit file, commit, push ke branch, dan buat PR.

## Spesifikasi Server

| Komponen | Minimum | Rekomendasi |
|----------|---------|-------------|
| RAM | 4 GB | 8 GB+ |
| CPU | 2 core | 4 core |
| Disk | 20 GB | 50 GB+ |
| OS | Ubuntu 20.04+ | Ubuntu 22.04 |

> **RAM 4 GB:** Setup swap 2 GB dan set `MAX_CONCURRENT_RUNS=3`.
> ```bash
> fallocate -l 2G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
> echo '/swapfile none swap sw 0 0' >> /etc/fstab
> ```

## Fitur Utama

- **AI Agent Coding via Chat** — kirim prompt dari API, Android app, atau Telegram bot
- **Branch Persistent per Project** — branch di-reuse antar session, baru hanya saat `/newsession`
- **Worktree Ephemeral per Run** — git worktree terpisah per run, cleanup otomatis
- **Notifikasi Telegram Otomatis** — "⏳ sedang bekerja" saat mulai, hasil lengkap saat selesai
- **Auto PR** — setelah push branch, PR dibuat otomatis via GitHub API
- **Run Timeout** — setiap run dibatasi 10 menit (configurable)
- **Stuck Run Recovery** — worker otomatis reset run yang stuck
- **Concurrency Guard** — hanya 1 run aktif per session
- **Rate Limiting per User** — batasi request per menit
- **Disk Space Alert** — notifikasi Telegram ke admin saat disk ≥ 85% (configurable)
- **Scheduled Daily Backup** — backup DB otomatis tiap malam, kirim ke Telegram admin
- **DB Backup via Telegram** — admin bisa trigger backup manual langsung dari chat
- **Redis Pub/Sub** — instant run pickup tanpa polling
- **Realtime WebSocket** — streaming event ke Android app
- **Policy Engine** — kontrol apa yang boleh dilakukan agent
- **Multi-Project** — kelola banyak project dalam satu platform
- **GitHub Integration** — clone private repo, push branch, create PR via API
- **Usage & Cost Tracking** — catat token dan estimasi biaya per run
- **API Key Auth** — SHA-256 hash, plain text tidak tersimpan
- **Log Rotation** — semua container punya limit log otomatis
- **Token Security** — GitHub token dipass via env var, tidak visible di `ps aux`
- **XML-Structured System Prompt** — prompt optimal dengan XML tags (identity/principles/constraints/skills) untuk output AI yang lebih akurat dan konsisten
- **On-Demand Skills** — 11 coding standards (OWASP, SOLID, deployment, dll) yang dibaca agent saat dibutuhkan, hemat token
- **Honest & Objective AI** — agent tidak yes-man, memberikan kritik konstruktif dan alternatif lebih baik
- **MCP Context7 Ready** — integrasi dokumentasi library real-time via Model Context Protocol
- **Docker Build Cache Cleanup** — cron mingguan otomatis prune build cache (mencegah disk penuh)
- **Markdown Fallback** — jika Telegram reject Markdown formatting, otomatis fallback ke plain text
- **Split Polling/API Client** — polling client (35s) terpisah dari API client (10s), bot tidak mati setelah run selesai

## Skills (AI Coding Standards)

Skills adalah kumpulan standar coding yang dibaca agent on-demand saat mengerjakan task. Tidak di-load semua sekaligus (hemat token), hanya yang relevan.

| Skill | Deskripsi |
|-------|-----------|
| `api-standards` | Response format, HTTP codes, URL naming, OWASP API |
| `auth-standards` | JWT, password hashing, session, RBAC/IDOR |
| `coding-standards` | SOLID, clean code, schema validation, N+1 |
| `config-standards` | .env, database connection, CORS |
| `dealtech-ui` | React components (shadcn/ui pattern) |
| `deployment-standards` | Docker caching, Coolify, CI/CD |
| `frontend-performance-seo` | Core Web Vitals, SEO, Accessibility |
| `security-standards` | Rate limiting, headers, injection, file upload |
| `cloudflare-turnstile` | Bot protection via Cloudflare Turnstile |
| `license-dealone` | Integrasi license key DealOne API |
| `project-structure` | Folder layout multi-stack |

### Cara Pakai

User cukup bilang di prompt:
```
"Buatkan REST API auth dengan JWT, baca skills"
```

Agent otomatis baca `auth-standards` dan `api-standards` lalu ikuti standarnya.

### Update Skills

```bash
cd /srv/ai-platform/skills && git pull
```

Tidak perlu rebuild Docker — skills di-mount sebagai volume read-only.

## Arsitektur

```
Android App / Telegram Bot
    ↓  HTTPS + WebSocket / Long Polling
Rust Backend (Axum + Tokio)
    ├── HTTP API (REST + WebSocket)
    ├── Telegram Bot (long polling)
    ├── Agent Run Worker (Redis pub/sub)
    ├── Stuck Run Recovery Worker
    ├── Cleanup Worker (worktree)
    ├── Disk Alert Worker (hourly)
    └── Backup Worker (daily 01:00 WIB)
    ↓  Private HTTP/SSE
OpenClaw Gateway → AI Provider (Claude / GPT / dll)
    ↓  Sandbox
Docker Container → Git Worktree → branch → edit → commit → push → PR
```

## Cara Kerja

1. User kirim prompt via Telegram atau API
2. Backend buat run (status `queued`), publish ke Redis
3. Worker pickup via Redis pub/sub
4. Clone/fetch repo, buat git worktree dari session branch
5. Kirim prompt + context (max 20 pesan terakhir) ke OpenClaw
6. Agent edit file di worktree
7. Commit (author: Dealtech Code), push ke branch, buat PR otomatis
8. Kirim hasil + link PR ke user via Telegram atau WebSocket
9. Cleanup worktree (branch tetap ada di GitHub)

## Konfigurasi

File: `/srv/ai-platform/.env`

| Variable | Default | Keterangan |
|---|---|---|
| `APP_PORT` | `8080` | Port HTTP server |
| `APP_ENV` | `development` | Environment |
| `LOG_LEVEL` | `info` | Level logging |
| `DB_HOST` | `localhost` | PostgreSQL host |
| `DB_PORT` | `5432` | PostgreSQL port |
| `DB_NAME` | `aicode` | Nama database |
| `DB_USER` | `postgres` | Database user |
| `DB_PASSWORD` | (required) | Database password |
| `REDIS_HOST` | `localhost` | Redis host |
| `REDIS_PORT` | `6379` | Redis port |
| `GITHUB_TOKEN` | (optional) | GitHub PAT — scope `repo` untuk clone/push/PR |
| `OPENCLAW_BASE_URL` | `http://127.0.0.1:18789` | URL OpenClaw Gateway |
| `OPENCLAW_GATEWAY_TOKEN` | (required) | Token autentikasi OpenClaw |
| `WORKSPACES_PATH` | `/srv/ai-platform/workspaces` | Path repo clone |
| `WORKTREES_PATH` | `/srv/ai-platform/worktrees` | Path worktree ephemeral |
| `LOGS_PATH` | `/srv/ai-platform/logs` | Path log files |
| `MAX_CONCURRENT_RUNS` | `5` | Maksimal run paralel |
| `OPENCLAW_MAX_RETRIES` | `3` | Retry OpenClaw call |
| `USER_RATE_LIMIT_PER_MINUTE` | `10` | Max request per user per menit |
| `TELEGRAM_BOT_TOKEN` | (empty) | Token bot dari @BotFather |
| `TELEGRAM_ENABLED` | `false` | Aktifkan Telegram bot |
| `TELEGRAM_ADMIN_CHAT_ID` | (optional) | Chat ID admin untuk disk alert & backup |
| `DISK_ALERT_THRESHOLD_PCT` | `85` | Alert saat disk ≥ nilai ini (%) |
| `BACKUP_HOUR_UTC` | `18` | Jam backup harian UTC (18 = 01:00 WIB) |
| `CORS_ORIGIN` | `*` | Allowed origins, pisah koma untuk restrict |

> **GitHub Token Scope:** Untuk auto PR, token perlu scope `repo`. Untuk repo public saja, `public_repo` cukup.

> **TELEGRAM_ADMIN_CHAT_ID:** Dapatkan chat ID kamu dengan kirim pesan ke bot lalu cek `https://api.telegram.org/bot<TOKEN>/getUpdates`.

## Telegram Bot

### Setup

1. Buat bot via [@BotFather](https://t.me/BotFather), dapatkan token
2. Tambahkan ke `.env`:
   ```bash
   TELEGRAM_BOT_TOKEN=***
   TELEGRAM_ENABLED=true
   TELEGRAM_ADMIN_CHAT_ID=123456789
   ```
3. Restart: `cd /srv/ai-platform && docker compose up -d --no-deps backend`

### Whitelist

Hanya user terdaftar di `telegram_users` yang bisa pakai bot. User pertama otomatis jadi admin.

### Command

| Command | Siapa | Keterangan |
|---|---|---|
| `/start` | Semua | Mulai, tampilkan project aktif |
| `/projects` | Semua | Daftar project |
| `/project <slug>` | Semua | Pilih project aktif |
| `/newproject <nama> <repo_url>` | Semua | Buat project baru |
| `/runs` | Semua | 10 run terakhir (status, branch, commit, cost) |
| `/newsession` | Semua | Mulai session baru dengan branch baru |
| `/retry` | Semua | Ulangi run terakhir yang gagal |
| `/cancel` | Semua | Batalkan run yang sedang berjalan |
| `/status` | Semua | Status saat ini (admin dapat info lebih lengkap) |
| `/help` | Semua | Tampilkan bantuan |
| `/adduser <telegram_id> <nama>` | Admin | Tambah user |
| `/removeuser <telegram_id>` | Admin | Hapus user |
| `/backup` | Admin | Backup DB manual, kirim file ke chat |

### `/status` — User vs Admin

**User biasa:**
```
📊 Status
👤 User: Riski
📂 Project: laziznu (laziznu)
🌿 Branch: ai/session-0b140b65
⏳ Run aktif: running_agent

📅 Runs hari ini: 3
💰 Cost hari ini: $0.0142
```

**Admin:**
```
📊 Status
[... info user ...]

─── Admin Panel ───
👥 Total users: 5
📁 Total projects: 3
⚡ Active runs: 1
📅 Runs hari ini (semua): 12
💰 Cost hari ini (semua): $0.0891
💳 Total cost all-time: $1.2340
💾 Disk: 🟢 Workspaces: 23% | 🟢 Worktrees: 8%
```

### Alur Penggunaan

1. Admin tambah user: `/adduser 123456789 Nama`
2. User buat project: `/newproject my-api https://github.com/org/my-api`
3. Kirim prompt: `"Tambahkan endpoint health check"`
4. Bot balas: `"⏳ Agent sedang bekerja..."`
5. Setelah selesai: hasil + link PR dikirim otomatis

## API

### Autentikasi

```
X-API-Key: ***
```

### Endpoint

| Method | Path | Keterangan |
|---|---|---|
| `GET` | `/health` | Health check — returns `{"status","db","redis"}` |
| `POST` | `/api/apikeys` | Buat API key (admin) |
| `GET` | `/api/apikeys` | List API keys |
| `POST` | `/api/apikeys/:id/revoke` | Revoke API key |
| `POST` | `/api/projects` | Buat project |
| `GET` | `/api/projects` | List projects |
| `GET` | `/api/projects/:id` | Detail project |
| `PATCH` | `/api/projects/:id` | Update project |
| `GET/PATCH` | `/api/projects/:id/policy` | Get/update policy |
| `POST` | `/api/projects/:pid/sessions` | Buat coding session |
| `GET` | `/api/projects/:pid/sessions` | List sessions |
| `GET` | `/api/sessions/:id/messages` | Riwayat pesan |
| `POST` | `/api/sessions/:sid/agent-runs` | Mulai agent run |
| `GET` | `/api/agent-runs/:id` | Status run |
| `POST` | `/api/agent-runs/:id/cancel` | Cancel run |
| `GET` | `/api/agent-runs/:id/diff` | Diff hasil run |
| `GET` | `/api/agent-runs/:id/events` | Event stream |
| `GET` | `/api/usage` | Usage & cost summary |
| `WS` | `/ws?api_key=***` | WebSocket realtime |

## Instalasi

```bash
git clone https://github.com/cahrur/dealtech-code.git
cd dealtech-code
chmod +x install.sh
./install.sh
```

Installer akan:
- Install Docker, dependencies
- Setup PostgreSQL, Redis, Caddy
- Buat symlink backend ke repo (no drift)
- Konfigurasi Telegram bot + admin chat ID (opsional)
- Setup SSL otomatis via Caddy
- Konfigurasi disk alert dan scheduled backup

### Setup Skills & MCP (opsional)

```bash
./setup-extras.sh
```

Script ini otomatis:
- Clone `Deal-Tech/skills` ke `/srv/ai-platform/skills/`
- Setup MCP Context7 untuk OpenClaw (jika terinstall)

## Update

```bash
cd /root/dealtech-code && git pull
./update.sh
```

`update.sh` otomatis:
- Pull kode terbaru
- Tambahkan env vars baru yang missing ke `.env`
- Rebuild dan restart backend
- Jalankan migrasi DB baru

## Background Workers

| Worker | Interval | Fungsi |
|---|---|---|
| `agent_run_worker` | Real-time (Redis) | Pickup dan jalankan queued runs |
| `stuck_run_recovery` | Setiap 5 menit | Reset run yang stuck/timeout |
| `cleanup_worker` | Setiap jam | Hapus worktree lama (>7 hari) |
| `disk_alert_worker` | Setiap jam | Alert Telegram saat disk ≥ threshold |
| `backup_worker` | Harian 01:00 WIB | Backup DB → kirim ke Telegram admin |

## Security

- **API key:** di-hash SHA-256, tidak pernah disimpan plaintext
- **GitHub token:** dipass via `GIT_CONFIG` env var — tidak visible di `ps aux`
- **SQL:** semua query parameterized — tidak ada injection risk
- **CORS:** configurable via `CORS_ORIGIN`
- **Telegram:** whitelist DB, admin check via DB role
- **Cleanup worker:** path traversal protection (hanya hapus path di bawah `WORKTREES_PATH`)
- **Rate limiting:** per user per menit (HTTP + Telegram)
- **Session history:** capped 20 pesan — mencegah context bloat dan cost tak terkendali

## Development

```bash
# Start dependencies
docker compose up -d postgres redis

# Run backend
cd backend && cargo run

# Tests
cargo test

# Lint
cargo clippy -- -D warnings
```

## License

Internal use only.
