# Dealtech Code — Panduan Integrasi Aplikasi

Dokumentasi lengkap untuk developer yang ingin mengintegrasikan aplikasi mobile/web dengan platform AI Coding Agent.

---

## Arsitektur Flow

```
Aplikasi Mobile
    ↓  HTTPS (REST API)
Backend (https://code.mudahdeal.com)
    ↓  Private HTTP/SSE
OpenClaw Gateway
    ↓  9router
AI Provider (Claude / GPT / dll)
    ↓  Sandbox
Docker Container (per user)
    ↓
Git Worktree → branch → test → commit → PR GitHub
```

---

## Flow Lengkap

```
1. Input API Key          → Autentikasi ke backend
2. Buat / pilih Project   → Terhubung ke repo GitHub
3. Buat Session           → Percakapan coding baru
4. Kirim Prompt           → Agent Run dimulai (async)
5. Subscribe WebSocket    → Terima event realtime
6. Lihat hasil            → diff, commit SHA, PR link
```

---

## Prerequisites

| Syarat | Keterangan |
|--------|-----------|
| API Key | Format `ak_xxx...` — dapatkan dari admin |
| Koneksi internet | Ke `https://code.mudahdeal.com` |
| WebSocket support | Untuk realtime event stream |
| GitHub repo | URL repo yang akan dikerjakan agent |

---

## Autentikasi

Semua endpoint (kecuali `/health`) memerlukan API Key.

**Header (pilih salah satu):**
```
X-API-Key: ak_your_key_here
```
```
Authorization: Bearer ak_your_key_here
```

**Contoh Kotlin — OkHttp Interceptor:**
```kotlin
val client = OkHttpClient.Builder()
    .addInterceptor { chain ->
        val req = chain.request().newBuilder()
            .header("X-API-Key", apiKey)
            .build()
        chain.proceed(req)
    }
    .build()
```

---

## Base URL

```
https://code.mudahdeal.com
```

---

## Response Format

**Success:**
```json
{
  "data": { ... },
  "success": true
}
```

**Error:**
```json
{
  "error": "Pesan error detail",
  "success": false
}
```

---

## HTTP Status Codes

| Code | Keterangan |
|------|-----------|
| 200 | OK |
| 201 | Created (resource berhasil dibuat) |
| 204 | No Content (berhasil, tidak ada body) |
| 400 | Bad Request (input tidak valid) |
| 401 | Unauthorized (API key tidak valid/expired) |
| 403 | Forbidden (tidak punya akses ke resource) |
| 404 | Not Found |
| 500 | Internal Server Error |

---

## Endpoints

### 1. Health Check

```
GET /health
```

Tidak memerlukan autentikasi. Digunakan untuk cek koneksi.

**Response 200:**
```
ok
```

---

### 2. Projects

Project adalah representasi dari satu repo GitHub yang akan dikerjakan agent.

#### 2.1 List Projects

```
GET /api/projects
```

**Response 200:**
```json
{
  "data": [
    {
      "id": "ae725b7f-d6c8-4113-a61f-0e64d964571d",
      "name": "Main API",
      "slug": "main-api",
      "repo_url": "https://github.com/org/repo.git",
      "openclaw_agent_id": "default",
      "description": "Backend utama",
      "created_at": "2026-05-23T08:00:00Z",
      "updated_at": "2026-05-23T08:00:00Z"
    }
  ],
  "success": true
}
```

---

#### 2.2 Buat Project

```
POST /api/projects
Content-Type: application/json
```

**Option A — dari repo yang sudah ada:**
```json
{
  "name": "Main API",
  "repo_url": "https://github.com/org/repo.git",
  "openclaw_agent_id": "default",
  "description": "Opsional"
}
```

**Option B — buat repo GitHub baru sekaligus:**
```json
{
  "name": "New Project",
  "openclaw_agent_id": "default",
  "description": "Opsional",
  "create_github_repo": true,
  "github_org": "nama-organisasi",
  "github_private": true
}
```

**Field:**

| Field | Type | Required | Keterangan |
|-------|------|----------|-----------|
| name | string | ✅ | Nama project |
| repo_url | string | ❌* | URL repo GitHub (HTTPS) |
| openclaw_agent_id | string | ✅ | Gunakan `"default"` |
| description | string | ❌ | Deskripsi project |
| create_github_repo | boolean | ❌ | `true` = buat repo baru di GitHub |
| github_org | string | ❌ | Nama org GitHub (jika repo di org) |
| github_private | boolean | ❌ | Private repo (default: `true`) |

*`repo_url` wajib jika `create_github_repo` tidak diset `true`.

**Response 201:**
```json
{
  "data": {
    "id": "ae725b7f-d6c8-4113-a61f-0e64d964571d",
    "name": "Main API",
    "slug": "main-api",
    "repo_url": "https://github.com/org/repo.git",
    "openclaw_agent_id": "default",
    "description": "Backend utama",
    "created_at": "2026-05-23T08:00:00Z",
    "updated_at": "2026-05-23T08:00:00Z"
  },
  "success": true
}
```

---

#### 2.3 Get Project

```
GET /api/projects/{project_id}
```

**Response 200:** sama dengan format project di atas.

---

#### 2.4 Update Project

```
PATCH /api/projects/{project_id}
Content-Type: application/json
```

**Request Body (semua field opsional):**
```json
{
  "name": "Nama baru",
  "repo_url": "https://github.com/org/repo-baru.git",
  "openclaw_agent_id": "agent-baru",
  "description": "Deskripsi baru"
}
```

---

#### 2.5 Get Policy

```
GET /api/projects/{project_id}/policy
```

**Response 200:**
```json
{
  "data": {
    "auto_mode": "auto_trusted",
    "policy": {
      "limits": {
        "max_run_minutes": 30,
        "max_retries": 3,
        "max_changed_files": 30
      },
      "git": {
        "auto_commit": true,
        "auto_push_branch": true,
        "auto_create_pr": true,
        "auto_merge": false
      },
      "network": {
        "default": "none",
        "allowed_hosts": ["registry.npmjs.org", "pypi.org", "crates.io"]
      }
    }
  },
  "success": true
}
```

---

#### 2.6 Update Policy

```
PATCH /api/projects/{project_id}/policy
Content-Type: application/json
```

**Request Body:**
```json
{
  "policy": {
    "auto_mode": "auto_trusted",
    "limits": { "max_run_minutes": 30, "max_retries": 3 },
    "git": {
      "auto_commit": true,
      "auto_push_branch": true,
      "auto_create_pr": true,
      "auto_merge": false
    }
  }
}
```

**Auto Mode:**

| Mode | Edit | Commit | Push | Install Deps |
|------|------|--------|------|-------------|
| `auto_safe` | ✅ | ❌ | ❌ | ❌ |
| `auto_trusted` | ✅ | ✅ | ✅ | ✅ (allowlist) |
| `auto_full` | ✅ | ✅ | ✅ | ✅ |

---

#### 2.7 Audit Logs

```
GET /api/projects/{project_id}/audit-logs
```

---

### 3. Sessions

Session adalah satu sesi percakapan coding dalam sebuah project.

#### 3.1 Buat Session

```
POST /api/projects/{project_id}/sessions
Content-Type: application/json
```

**Request Body:**
```json
{
  "title": "Fix bug login"
}
```

| Field | Type | Required | Keterangan |
|-------|------|----------|-----------|
| title | string | ❌ | Judul session (default: "New Session") |

**Response 201:**
```json
{
  "data": {
    "id": "b3f1a2c4-...",
    "project_id": "ae725b7f-...",
    "user_id": "513503b7-...",
    "title": "Fix bug login",
    "created_at": "2026-05-23T08:10:00Z",
    "updated_at": "2026-05-23T08:10:00Z"
  },
  "success": true
}
```

---

#### 3.2 List Sessions

```
GET /api/projects/{project_id}/sessions
```

---

#### 3.3 Get Session

```
GET /api/sessions/{session_id}
```

---

#### 3.4 Get Messages

```
GET /api/sessions/{session_id}/messages
```

**Response 200:**
```json
{
  "data": [
    {
      "id": "uuid",
      "session_id": "uuid",
      "role": "user",
      "content": "Buatkan endpoint health check",
      "created_at": "2026-05-23T08:10:00Z"
    }
  ],
  "success": true
}
```

---

### 4. Agent Runs

#### 4.1 Kirim Prompt ke Agent

Endpoint utama — user kirim prompt, agent bekerja otomatis.

```
POST /api/sessions/{session_id}/agent-runs
Content-Type: application/json
```

**Request Body:**
```json
{
  "prompt": "Buatkan endpoint health check dengan unit test",
  "auto_mode": "auto_trusted",
  "model": "claude-sonnet-4-6"
}
```

| Field | Type | Required | Keterangan |
|-------|------|----------|-----------|
| prompt | string | ✅ | Instruksi untuk agent |
| auto_mode | string | ❌ | Mode otomatis (default: `auto_trusted`) |
| model | string | ❌ | Model AI (default: `claude-sonnet-4-6`) |

**Model:** `claude-opus-4-7` (powerful) · `claude-sonnet-4-6` (recommended) · `claude-haiku-4-5` (cepat) · `gpt-4o` · `gpt-4o-mini`

**Response 201:**
```json
{
  "data": {
    "id": "run-uuid",
    "session_id": "session-uuid",
    "project_id": "project-uuid",
    "prompt": "Buatkan endpoint health check",
    "status": "queued",
    "auto_mode": "auto_trusted",
    "model": "claude-sonnet-4-6",
    "branch_name": null,
    "commit_sha": null,
    "pr_url": null,
    "started_at": null,
    "finished_at": null,
    "created_at": "2026-05-23T08:15:00Z"
  },
  "success": true
}
```

---

#### 4.2 Get Status Run

```
GET /api/agent-runs/{run_id}
```

**Status:**

| Status | Keterangan |
|--------|-----------|
| `queued` | Menunggu diproses |
| `preparing_workspace` | Menyiapkan workspace |
| `running_agent` | Agent sedang bekerja |
| `collecting_diff` | Mengumpulkan perubahan |
| `running_tests` | Menjalankan test |
| `auto_commit` | Membuat commit |
| `auto_push_or_pr` | Push branch & buat PR |
| `completed` | Selesai sukses |
| `failed_agent` | Agent gagal |
| `failed_tests` | Test gagal |
| `blocked_by_policy` | Diblok policy |
| `cancelled` | Dibatalkan |
| `timed_out` | Timeout |

---

#### 4.3 Cancel Run

```
POST /api/agent-runs/{run_id}/cancel
```

#### 4.4 Get Diff

```
GET /api/agent-runs/{run_id}/diff
```

**Response 200:**
```json
{
  "data": { "diff": "diff --git a/src/main.rs..." },
  "success": true
}
```

#### 4.5 Get Events (Catch-up setelah reconnect)

```
GET /api/agent-runs/{run_id}/events?after_seq=0
```

---

### 5. Usage Tracking

```
GET /api/usage
GET /api/usage/logs?limit=50
```

**Response GET /api/usage:**
```json
{
  "data": {
    "total_runs": 15,
    "total_input_tokens": 45000,
    "total_output_tokens": 12000,
    "total_tokens": 57000,
    "by_model": [
      { "model": "claude-sonnet-4-6", "runs": 12, "input_tokens": 38000, "output_tokens": 10000 }
    ]
  },
  "success": true
}
```

---

### 6. WebSocket — Realtime Events

**Koneksi:**
```
wss://code.mudahdeal.com/ws?api_key=ak_your_key
```

**Subscribe ke session:**
```json
{ "type": "subscribe_session", "session_id": "uuid", "after_seq": 0 }
```

**Ping/Pong:** `{ "type": "ping" }` → `{ "type": "pong" }`

**Event Types:**

| Event | Keterangan |
|-------|-----------|
| `agent_run.started` | Run dimulai |
| `assistant.delta` | Token stream dari AI — tampilkan ke user |
| `tool.started` | Tool/command dimulai |
| `terminal.output` | Output stdout/stderr |
| `policy.blocked` | Command diblok policy |
| `file.changed` | File diubah |
| `tests.started` | Test dimulai |
| `tests.finished` | Test selesai |
| `git.committed` | Commit dibuat |
| `pr.created` | PR dibuat di GitHub |
| `agent_run.completed` | Run selesai sukses |
| `agent_run.failed` | Run gagal |

**Format Event:**
```json
{
  "type": "assistant.delta",
  "seq": 42,
  "session_id": "uuid",
  "run_id": "uuid",
  "data": { "delta": "Menambahkan endpoint..." }
}
```

---

### 7. API Keys (Admin Only)

#### 7.1 List

```
GET /api/apikeys
```

#### 7.2 Buat

```
POST /api/apikeys
```

```json
{ "name": "Mobile App", "role": "developer", "expires_at": null }
```

| Role | Akses |
|------|-------|
| `admin` | Semua endpoint + kelola API key |
| `developer` | Buat project, session, agent run |
| `viewer` | Hanya baca |

**Response 201:**
```json
{
  "id": "uuid",
  "name": "Mobile App",
  "key": "ak_xxx...",
  "key_prefix": "ak_xxx",
  "role": "developer",
  "created_at": "2026-05-23T08:00:00Z"
}
```

> ⚠️ **`key` hanya tampil SEKALI. Simpan segera.**

#### 7.3 Revoke

```
POST /api/apikeys/{key_id}/revoke
```

---

## Contoh Flow Lengkap (Kotlin)

```kotlin
val apiKey = "ak_oJGNsdh2jCHlumoVw368EeMwo1TbsQ1Px9S94OjhvIBDU7M0"

// 1. Buat project (sekali saja, simpan project_id)
val project = api.createProject(CreateProjectRequest(
    name = "My App",
    repoUrl = "https://github.com/org/my-app.git",
    openclawAgentId = "default"
))

// 2. Buat session untuk setiap topik coding
val session = api.createSession(
    projectId = project.data.id,
    CreateSessionRequest(title = "Fix login bug")
)

// 3. Connect WebSocket untuk realtime stream
val ws = OkHttpClient().newWebSocket(
    Request.Builder()
        .url("wss://code.mudahdeal.com/ws?api_key=$apiKey")
        .build(),
    object : WebSocketListener() {
        override fun onOpen(ws: WebSocket, response: Response) {
            ws.send("""{"type":"subscribe_session","session_id":"${session.data.id}","after_seq":0}""")
        }
        override fun onMessage(ws: WebSocket, text: String) {
            val event = Json.decodeFromString<AgentEvent>(text)
            when (event.type) {
                "assistant.delta"     -> appendToChat(event.data?.delta)
                "file.changed"        -> showChangedFiles(event.files)
                "pr.created"          -> showPRLink(event.branch)
                "agent_run.completed" -> showDoneUI()
                "agent_run.failed"    -> showErrorUI()
            }
        }
    }
)

// 4. Kirim prompt — setiap kali user chat
val run = api.createAgentRun(
    sessionId = session.data.id,
    CreateRunRequest(
        prompt = "Fix bug di login flow, tambahkan error handling",
        autoMode = "auto_trusted",
        model = "claude-sonnet-4-6"
    )
)
```

---

## Error Handling

```kotlin
try {
    val response = api.createProject(request)
} catch (e: HttpException) {
    when (e.code()) {
        401 -> showError("API key tidak valid")
        403 -> showError("Tidak punya akses")
        404 -> showError("Resource tidak ditemukan")
        400 -> showError("Request tidak valid: ${e.message}")
        else -> showError("Server error, coba lagi")
    }
}
```

---

## Reconnect WebSocket

```kotlin
var lastSeq = 0L

override fun onMessage(ws: WebSocket, text: String) {
    val event = Json.decodeFromString<AgentEvent>(text)
    lastSeq = event.seq
    handleEvent(event)
}

override fun onFailure(ws: WebSocket, t: Throwable, response: Response?) {
    reconnectWithBackoff {
        ws.send("""{"type":"subscribe_session","session_id":"$sessionId","after_seq":$lastSeq}""")
    }
}

// Atau catch-up via REST setelah reconnect
val missed = api.getEvents(runId, afterSeq = lastSeq)
missed.data.forEach { handleEvent(it) }
