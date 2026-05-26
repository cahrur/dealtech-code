# Dealtech Code — AI Coding Agent Platform

Platform AI coding agent yang memungkinkan tim developer berkolaborasi dengan AI untuk mengerjakan proyek coding langsung di repository GitHub. Kirim prompt dari Android app atau Telegram, agent AI bekerja otomatis — edit file, jalankan test, commit, dan push ke branch.

## Spesifikasi Server yang Direkomendasikan

| Komponen | Minimum | Rekomendasi |
|----------|---------|-------------|
| RAM | 4 GB | 8 GB+ |
| CPU | 2 core | 4 core |
| Disk | 20 GB | 50 GB+ |
| OS | Ubuntu 20.04+ | Ubuntu 22.04 |

> **Penting untuk RAM 4 GB:** Wajib setup swap 2 GB sebagai safety net:
> ```bash
> fallocate -l 2G /swapfile
> chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
> echo '/swapfile none swap sw 0 0' >> /etc/fstab
> ```
> Set `MAX_CONCURRENT_RUNS=3` untuk RAM 4 GB (default). Naikkan ke 5 untuk RAM 8 GB+.

## Fitur Utama

- **AI Agent Coding via Chat** — kirim prompt coding dari API, Android app, atau Telegram bot
- **1 Branch per Session** — setiap coding session punya branch persistent yang di-reuse antar run
- **Worktree Ephemeral per Run** — setiap run menggunakan git worktree terpisah, cleanup otomatis setelah selesai
- **Run Timeout** — setiap run dibatasi 10 menit (configurable), auto-cancel jika melebihi batas
- **Stuck Run Recovery** — worker otomatis reset run yang stuck setiap 60 detik
- **Concurrency Guard per Session** — hanya 1 run aktif per session, mencegah konflik
- **Max Concurrent Runs Semaphore** — batasi total run paralel dengan `MAX_CONCURRENT_RUNS`
- **OpenClaw Retry dengan Exponential Backoff** — retry otomatis saat OpenClaw gagal (max `OPENCLAW_MAX_RETRIES`)
- **Rate Limiting per User** — batasi request per menit dengan `USER_RATE_LIMIT_PER_MINUTE`
- **Run Duration Metrics Logging** — setiap run dicatat durasi eksekusinya
- **Graceful Shutdown** — handle SIGTERM/SIGINT dengan baik, tunggu run selesai sebelum shutdown
- **Redis Pub/Sub** — instant run pickup tanpa polling, worker langsung proses run baru
- **Telegram Bot** — whitelist-based, auto-route ke project, command lengkap
- **Realtime WebSocket** — streaming event ke Android app (token stream, file changed, commit, PR)
- **Auto Commit & Push** — agent otomatis commit dan push ke branch sesuai policy
- **Policy Engine** — kontrol apa yang boleh dilakukan agent (commit, push, install deps)
- **Multi-Project** — kelola banyak project dalam satu platform
- **GitHub Integration** — clone private repo, push branch, create repo baru via API
- **Usage Tracking** — catat penggunaan token per user per model
- **API Key Auth** — autentikasi via API key (hash SHA-256, plain text tidak tersimpan)
- **Per-user Docker Container** — isolasi runtime per API key

## Arsitektur

```
Android App / Telegram Bot
    ↓  HTTPS + WebSocket / Long Polling
Rust Backend (Axum + Tokio)
    ├── HTTP API (REST + WebSocket)
    ├── Telegram Bot (long polling)
    ├── Agent Run Worker (Redis pub/sub)
    ├── Stuck Run Recovery Worker
    └── Cleanup Worker
    ↓  Private HTTP/SSE
OpenClaw Gateway
    ↓  9router
AI Provider (Claude / GPT / dll)
    ↓  Sandbox
Docker Container (1 per API key)
    ↓
Git Worktree → branch → edit → test → commit → push
```

### Komponen Utama

| Komponen | Teknologi | Fungsi |
|---|---|---|
| Backend | Rust + Axum + Tokio + SQLx | HTTP API, WebSocket, worker, Telegram bot |
| Database | PostgreSQL 16 | Data persistence (users, projects, sessions, runs) |
| Cache/Queue | Redis 7 | Pub/sub untuk instant run pickup, caching |
| Agent Runtime | OpenClaw Gateway | Eksekusi AI agent di sandbox |
| AI Router | 9router | Multi-provider routing (Anthropic, OpenAI, dll) |
| Reverse Proxy | Caddy | Auto TLS, routing |
| Container | Docker | Isolasi per user |

## Cara Kerja

1. **User kirim prompt** — via Android app (REST API), Telegram bot, atau WebSocket
2. **Backend buat run** — insert ke DB dengan status `queued`, publish ke Redis channel
3. **Worker pickup** — agent run worker terima notifikasi via Redis pub/sub
4. **Prepare workspace** — clone/fetch repo, buat git worktree dari session branch
5. **Jalankan agent** — kirim prompt + context ke OpenClaw Gateway via SSE
6. **Agent bekerja** — AI edit file, jalankan command di worktree
7. **Collect hasil** — ambil diff, list changed files
8. **Auto commit & push** — sesuai policy, commit perubahan dan push ke branch
9. **Kirim reply** — balas ke user via API response, WebSocket event, atau Telegram message
10. **Cleanup** — hapus worktree (branch tetap ada untuk session berikutnya)

## Konfigurasi

Semua config via environment variable di `/srv/ai-platform/.env`:

| Variable | Default | Keterangan |
|---|---|---|
| `APP_PORT` | `8080` | Port HTTP server |
| `APP_ENV` | `development` | Environment (development/production) |
| `LOG_LEVEL` | `info` | Level logging |
| `DB_HOST` | `localhost` | PostgreSQL host |
| `DB_PORT` | `5432` | PostgreSQL port |
| `DB_NAME` | `aicode` | Nama database |
| `DB_USER` | `postgres` | Database user |
| `DB_PASSWORD` | (required) | Database password |
| `REDIS_HOST` | `localhost` | Redis host |
| `REDIS_PORT` | `6379` | Redis port |
| `ADMIN_API_KEY` | (optional) | API key admin untuk bootstrap |
| `GITHUB_TOKEN` | (optional) | GitHub PAT untuk clone/push private repo |
| `OPENCLAW_BASE_URL` | `http://127.0.0.1:18789` | URL OpenClaw Gateway |
| `OPENCLAW_GATEWAY_TOKEN` | (required) | Token autentikasi OpenClaw |
| `WORKSPACES_PATH` | `/srv/ai-platform/workspaces` | Path penyimpanan repo clone |
| `WORKTREES_PATH` | `/srv/ai-platform/worktrees` | Path worktree ephemeral |
| `LOGS_PATH` | `/srv/ai-platform/logs` | Path log files |
| `GRACEFUL_SHUTDOWN` | `true` | Aktifkan graceful shutdown |
| `MAX_CONCURRENT_RUNS` | `5` | Maksimal run paralel |
| `OPENCLAW_MAX_RETRIES` | `3` | Retry OpenClaw call |
| `USER_RATE_LIMIT_PER_MINUTE` | `10` | Max request per user per menit |
| `TELEGRAM_BOT_TOKEN` | (empty) | Token bot Telegram dari @BotFather |
| `TELEGRAM_ENABLED` | `false` | Aktifkan Telegram bot |

## Telegram Bot

### Setup

1. Buat bot baru di Telegram via [@BotFather](https://t.me/BotFather)
2. Dapatkan token bot
3. Tambahkan ke environment:
   ```bash
   echo "TELEGRAM_BOT_TOKEN=123456:ABC-DEF" >> /srv/ai-platform/.env
   echo "TELEGRAM_ENABLED=true" >> /srv/ai-platform/.env
   ```
4. Restart backend:
   ```bash
   cd /srv/ai-platform && docker compose up -d --no-deps backend
   ```

### Whitelist

Bot menggunakan sistem whitelist. Hanya user yang terdaftar di tabel `telegram_users` yang bisa menggunakan bot. User pertama yang ditambahkan otomatis menjadi admin.

### Command

| Command | Keterangan |
|---|---|
| `/start` | Mulai, tampilkan project aktif |
| `/projects` | Lihat daftar project yang bisa diakses |
| `/project <slug>` | Pilih project aktif |
| `/newproject <nama> <repo_url>` | Buat project baru dan set sebagai aktif |
| `/status` | Tampilkan status saat ini |
| `/help` | Tampilkan bantuan |
| `/adduser <telegram_id> <nama>` | (Admin) Tambah user ke whitelist |
| `/removeuser <telegram_id>` | (Admin) Hapus user dari whitelist |

### Penggunaan

1. Admin tambahkan user: `/adduser 123456789 Nama`
2. User pilih project: `/project my-api`
3. Kirim pesan biasa untuk memulai coding: "Tambahkan endpoint health check"
4. Bot akan memproses dan mengirim hasil (termasuk diff dan status push)

Session otomatis dibuat per hari per user per project. Pesan dalam hari yang sama akan reuse session yang sama.

## API

### Autentikasi

Semua endpoint protected memerlukan header:
```
X-API-Key: ak_your_key_here
```

### Endpoint Utama

| Method | Path | Keterangan |
|---|---|---|
| `POST` | `/api/apikeys` | Buat API key baru (admin) |
| `GET` | `/api/apikeys` | List API keys |
| `POST` | `/api/apikeys/:id/revoke` | Revoke API key |
| `POST` | `/api/projects` | Buat project baru |
| `GET` | `/api/projects` | List projects |
| `GET` | `/api/projects/:id` | Detail project |
| `PATCH` | `/api/projects/:id` | Update project |
| `GET/PATCH` | `/api/projects/:id/policy` | Get/update policy |
| `POST` | `/api/projects/:pid/sessions` | Buat coding session |
| `GET` | `/api/projects/:pid/sessions` | List sessions |
| `GET` | `/api/sessions/:id/messages` | Riwayat pesan session |
| `POST` | `/api/sessions/:sid/agent-runs` | Mulai agent run |
| `GET` | `/api/agent-runs/:id` | Status run |
| `POST` | `/api/agent-runs/:id/cancel` | Cancel run |
| `GET` | `/api/agent-runs/:id/diff` | Diff hasil run |
| `GET` | `/api/agent-runs/:id/events` | Event stream run |
| `GET` | `/api/usage` | Usage summary |
| `WS` | `/ws?api_key=ak_xxx` | WebSocket realtime |

## Development

### Prerequisites

- Rust 1.88+
- Docker & Docker Compose
- PostgreSQL 16
- Redis 7

### Build & Run Lokal

```bash
# Start dependencies
docker compose up -d postgres redis

# Run backend
cd backend && cargo run

# Run tests
cargo test

# Lint
cargo clippy -- -D warnings
```

### Build Docker Image

```bash
cd /root/dealtech-code
docker build -f backend/Dockerfile -t ai-platform-backend backend/
```

### Deploy

```bash
cd /srv/ai-platform && docker compose up -d --no-deps backend
```

## License

Internal use only.

