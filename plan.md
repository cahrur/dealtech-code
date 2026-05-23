Blueprint Internal AI Coding Agent Platform
Kotlin Mobile + Rust Backend + OpenClaw Runtime + Auto Mode
Rencana arsitektur, workflow, security, API, database, deployment, dan roadmap implementasi
Versi 1.0 - 23 Mei 2026
Target: internal team kecil, realtime, remote dari API/mobile, multi-user, auto-run tanpa approval UI
 
Revisi dari requirement terakhir
No	Revisi	Keputusan Desain
1	Mobile app pakai Kotlin	Gunakan Android native: Kotlin, Jetpack Compose, Ktor/OkHttp WebSocket, Room, DataStore, Hilt.
2	Backend utama Rust	Gunakan Rust sebagai pilihan utama: Axum, Tokio, SQLx, Postgres, Redis, reqwest, tracing. Go/Python tetap dicatat sebagai fallback.
3	Tidak perlu approval di app	Gunakan Auto Mode: agent jalan otomatis di sandbox disposable, branch/worktree per run, policy otomatis, audit log, test gate, dan hard deny untuk hal berbahaya.

Catatan penting
Tidak ada approval UI bukan berarti semua command harus bebas. Desain yang aman adalah membuat setiap run disposable dan membatasi blast radius. Kalau agent gagal atau melakukan command destruktif, yang rusak hanya sandbox/worktree sementara, bukan host VPS, bukan main branch, dan bukan production secrets.
Daftar isi
•	1. Executive decision
•	2. Requirement dan batasan sistem
•	3. Arsitektur final
•	4. Stack teknis final
•	5. Android Kotlin app design
•	6. Rust backend wrapper design
•	7. OpenClaw integration design
•	8. Auto Mode tanpa approval UI
•	9. Workflow coding end-to-end
•	10. Multi-user dan concurrency
•	11. Git, workspace, dan sandbox lifecycle
•	12. API design dan WebSocket event protocol
•	13. Database schema
•	14. Deployment VPS dan Docker topology
•	15. Security hardening
•	16. Observability, backup, dan operasi harian
•	17. Roadmap implementasi
•	18. Checklist siap pakai
•	19. Appendix: config dan snippet
•	20. Referensi teknis
 
1. Executive decision
Keputusan final: pakai OpenClaw sebagai agent runtime internal, bukan sebagai public-facing backend. Mobile app Kotlin dan semua user/team hanya bicara ke Backend Wrapper. Backend Wrapper utama ditulis dengan Rust. OpenClaw Gateway hanya private network atau localhost. Semua eksekusi agent terjadi di sandbox disposable. Tidak ada approval UI; sistem memakai Auto Mode dengan policy otomatis, test gate, audit log, dan rollback path.
1.1 Formula sistem
Formula arsitektur
Android Kotlin App
  -> HTTPS + WebSocket
Rust Backend Wrapper
  -> private HTTP/SSE to OpenClaw
OpenClaw Gateway
  -> sandboxed tool execution
Disposable Workspace / Git Worktree
  -> branch -> test -> commit -> push PR
1.2 Kenapa bukan OpenClaw langsung ke mobile?
•	Token OpenClaw tidak boleh berada di mobile app atau browser. Mobile app bisa di-reverse-engineer dan token bisa bocor.
•	Backend perlu menjadi policy engine: auth, role, project permission, quota, audit log, queue, lock, dan auto-mode policy.
•	OpenClaw Gateway idealnya private. Dokumentasi OpenClaw menjelaskan Gateway biasanya bind ke loopback dan remote access lebih aman lewat SSH tunnel atau tailnet/VPN. Lihat [R3].
•	OpenClaw /v1/responses berguna untuk backend-to-gateway streaming, bukan untuk dipanggil langsung dari perangkat user. Lihat [R1].
1.3 Definisi sukses MVP
•	User login dari Android app.
•	User pilih project internal.
•	User kirim prompt coding.
•	Backend otomatis membuat agent run, branch, dan workspace sementara.
•	OpenClaw menjalankan agent di sandbox.
•	Progress dan output realtime masuk ke mobile via WebSocket.
•	Agent edit file, run test/lint, lalu menghasilkan diff.
•	Jika test pass, backend otomatis commit dan push branch / create PR sesuai policy project.
•	Semua command, output penting, file change, dan status terekam di audit log.
2. Requirement dan batasan sistem
Area	Requirement	Keputusan
Remote API	Bisa dikendalikan dari mobile/app/API	Semua command user masuk ke Rust Backend. Backend expose REST + WebSocket.
Realtime	Chat, log, test output, diff status realtime	Gunakan WebSocket app-level antara Android dan Rust Backend. Backend parse stream dari OpenClaw.
Multi-user	Internal team kecil	RBAC, project role, session rooms, project lock/worktree per run.
Auto	Tidak ada approval di app	Auto Mode: policy otomatis, hard deny, sandbox disposable, branch isolation, retry limit.
Mobile	Android Kotlin	Jetpack Compose, Ktor/OkHttp WS, Room cache, DataStore token, Hilt.
Backend	Utama Rust	Axum/Tokio/SQLx/Postgres/Redis/reqwest/tracing.
Agent runtime	Pakai OpenClaw	OpenClaw Gateway private, /v1/responses streaming, sandbox mode all/session.

2.1 Asumsi operasional
•	Team berukuran kecil: 2 sampai 15 orang.
•	Repo dikelola di GitHub/GitLab/Bitbucket atau bare Git internal.
•	Production deploy tidak dilakukan langsung oleh agent pada fase awal.
•	Agent boleh push branch dan create PR otomatis kalau test gate pass.
•	Main branch tidak pernah diedit langsung oleh agent.
•	Secrets production tidak pernah dimount ke workspace agent.
•	Setiap run bisa dihapus dan dibuat ulang tanpa merusak repo utama.
2.2 Non-goals fase awal
•	Tidak membuat public SaaS multi-tenant.
•	Tidak memberi shell host VPS ke user.
•	Tidak menjalankan production deployment otomatis tanpa environment terpisah.
•	Tidak membuat agent punya akses langsung ke database production.
•	Tidak menyimpan OpenClaw operator token di Android app.
3. Arsitektur final
High-level architecture
+---------------------------------------------------------------+
| Android Kotlin App                                             |
| - Login, project list, chat, run stream, diff viewer           |
| - WebSocket event stream                                       |
+-----------------------------+---------------------------------+
                              |
                              | HTTPS + WebSocket
                              v
+---------------------------------------------------------------+
| Rust Backend Wrapper                                           |
| - Auth / RBAC / project permission                             |
| - Session manager / run manager / queue                        |
| - Auto policy engine / audit log                               |
| - Git branch/worktree lifecycle                                |
| - OpenClaw SSE parser -> WebSocket fanout                      |
+-----------------------------+---------------------------------+
                              |
                              | Private HTTP/SSE
                              v
+---------------------------------------------------------------+
| OpenClaw Gateway                                               |
| - OpenResponses-compatible /v1/responses                       |
| - Agent routing / session routing                              |
| - Tool orchestration                                           |
| - Sandboxed exec/read/write/edit/apply_patch                   |
+-----------------------------+---------------------------------+
                              |
                              | Sandbox backend
                              v
+---------------------------------------------------------------+
| Disposable Workspace                                           |
| - Git worktree per run                                         |
| - Non-root container                                           |
| - Limited CPU/RAM/disk                                         |
| - No production secrets                                        |
+---------------------------------------------------------------+
3.1 Trust boundary
Layer	Trusted?	Boleh akses apa?	Tidak boleh akses apa?
Android app	Low trust	API via JWT, project data sesuai role	OpenClaw token, DB, Redis, host shell
Rust backend	Trusted app core	DB, Redis, OpenClaw private endpoint, Git provider token	Direct unsafe shell tanpa wrapper
OpenClaw Gateway	Trusted internal runtime	Agent workspace dan sandbox	Public internet exposure, mobile direct access
Sandbox	Untrusted execution zone	Temporary worktree, test deps	Host secrets, docker.sock, production DB

3.2 Mode network
•	Public only: domain app, HTTPS, WebSocket endpoint backend.
•	Private only: OpenClaw Gateway port, Postgres, Redis, sandbox network.
•	Gateway bind ke loopback atau Docker private network. Jangan publish ke internet.
•	Kalau butuh remote ops, pakai SSH tunnel, WireGuard, atau Tailscale, bukan public Gateway.
4. Stack teknis final
Komponen	Pilihan Utama	Alternatif	Catatan
Mobile	Kotlin + Jetpack Compose	Flutter/React Native tidak dipakai	Native Android lebih cocok untuk app internal stabil.
Networking mobile	Ktor Client atau OkHttp	Retrofit untuk REST	WebSocket untuk realtime. REST untuk data biasa.
Local storage	Room + DataStore	SQLDelight	Room untuk cache, DataStore untuk token/settings.
Backend	Rust + Axum + Tokio	Go/FastAPI	Rust dipilih untuk performa, safety, concurrency.
DB	Postgres	MySQL	Relational data, audit log, sessions.
Realtime	Axum WebSocket + Redis pub/sub	NATS	Redis cukup untuk internal team kecil.
Queue	Redis Streams / Apalis Redis	RabbitMQ	Agent run bisa durable dan retry.
Agent runtime	OpenClaw Gateway	Codex direct runner	OpenClaw mempercepat orchestration.
Sandbox	Docker sandbox per session/run	SSH sandbox / microVM later	Fase awal Docker cukup jika private internal.
Reverse proxy	Caddy	Nginx	Caddy simpel untuk TLS otomatis.

4.1 Rust crates rekomendasi
Kebutuhan	Crate	Fungsi
HTTP API	axum	REST endpoint dan WebSocket.
Async runtime	tokio	Runtime utama async.
DB	sqlx	Postgres query compile/runtime checked.
Redis	redis	Cache, pub/sub, lock, queue.
HTTP client	reqwest	Call OpenClaw /v1/responses dan Git provider API.
Serialization	serde, serde_json	JSON request/response/event.
Auth	jsonwebtoken, argon2	JWT dan password hash.
Tracing	tracing, tracing-subscriber	Structured logs.
Metrics	opentelemetry, metrics	Observability.
UUID/time	uuid, time/chrono	IDs dan timestamp.
Config	config, dotenvy	Environment config.
Git	tokio::process git CLI	Lebih transparan dan mudah diaudit daripada library untuk semua operasi.

4.2 Go/Python fallback
•	Go fallback: Gin/Echo/Fiber, pgx, go-redis, gorilla/websocket atau nhooyr.io/websocket, asynq untuk Redis jobs.
•	Python fallback: FastAPI, SQLAlchemy, asyncpg, Redis, Celery/ARQ, httpx, websockets.
•	Tetap rekomendasi utama: Rust. Go dipilih kalau tim lebih butuh velocity; Python dipilih kalau banyak eksperimen AI internal.
5. Android Kotlin app design
5.1 Modul Android
Android module layout
app/
  core-network/       REST client, WebSocket client, retry, auth interceptor
  core-database/      Room entities, DAO, local cache
  core-model/         DTO/domain model
  core-ui/            theme, components, code/diff views
  feature-auth/       login, token refresh
  feature-projects/   project list, project detail
  feature-session/    chat session, realtime stream
  feature-run/        agent run timeline, terminal output, diff summary
  feature-settings/   account, API endpoint, debug tools
5.2 Screen utama
Screen	Fungsi	Realtime?
Login	Email/password atau SSO; simpan token di encrypted storage	Tidak
Project List	Daftar project yang user boleh akses	Opsional
Project Detail	Branch, status, session terakhir, active run	Ya
Chat Session	Prompt coding, stream jawaban agent, timeline tool	Ya
Run Detail	Status run, command output, file changed, test result	Ya
Diff Viewer	Lihat perubahan file; read-only untuk MVP	Tidak wajib realtime
Settings	Endpoint, logout, debug logs	Tidak

5.3 Realtime Android
•	Gunakan satu WebSocket utama: /ws?token=... atau Authorization header jika client mendukung.
•	Client subscribe ke room session/project setelah connected.
•	Semua event punya seq number agar client bisa resume setelah reconnect.
•	Saat reconnect, app call GET /api/sessions/{id}/events?after_seq=N untuk catch-up.
•	Gunakan exponential backoff: 1s, 2s, 5s, 10s, maksimum 30s.
•	Simpan snapshot session dan message terakhir di Room agar app tetap usable saat sinyal buruk.
Contoh model event Kotlin
sealed interface AgentEvent {
  val seq: Long
  val sessionId: String
  val runId: String?
}

data class AssistantDelta(
  override val seq: Long,
  override val sessionId: String,
  override val runId: String,
  val delta: String
) : AgentEvent

data class ToolOutput(
  override val seq: Long,
  override val sessionId: String,
  override val runId: String,
  val stream: String,
  val data: String
) : AgentEvent
5.4 Security mobile
•	JWT/refresh token disimpan di Android Keystore atau EncryptedSharedPreferences/DataStore encrypted.
•	OpenClaw token tidak pernah dikirim ke mobile.
•	Mobile hanya mendapat project yang user punya permission.
•	Setiap mutation endpoint tetap dicek di backend; jangan percaya client-side role.
•	Untuk internal team, tambah device revoke list dan session logout all devices.
6. Rust backend wrapper design
6.1 Struktur service
Rust backend layout
backend/
  src/
    main.rs
    config.rs
    app_state.rs
    http/
      routes.rs
      auth_routes.rs
      project_routes.rs
      session_routes.rs
      run_routes.rs
      ws.rs
    domain/
      auth.rs
      project.rs
      session.rs
      agent_run.rs
      policy.rs
      audit.rs
    services/
      auth_service.rs
      project_service.rs
      openclaw_service.rs
      run_orchestrator.rs
      git_service.rs
      workspace_service.rs
      policy_engine.rs
      audit_service.rs
      realtime_service.rs
    infra/
      db.rs
      redis.rs
      queue.rs
      github.rs
      openclaw_client.rs
    workers/
      agent_run_worker.rs
      cleanup_worker.rs
      event_replay_worker.rs
6.2 Backend responsibilities
Service	Tanggung jawab
AuthService	Login, refresh token, password hash/SSO, revoke session.
ProjectService	Project CRUD, member role, repo URL, OpenClaw agent mapping.
SessionService	Chat session, message persistence, session key mapping.
RunOrchestrator	Membuat agent run, enqueue, state machine, timeout, retry.
OpenClawService	Call /v1/responses, parse SSE, normalize events.
WorkspaceService	Clone repo, create worktree, cleanup disposable workspace.
GitService	Fetch, branch, diff, commit, push, create PR.
PolicyEngine	Auto-mode rules, denylist, allowed operations, mode per project.
AuditService	Log user action, tool summary, file changes, policy decision.
RealtimeService	WebSocket rooms, Redis pub/sub, event sequence.

6.3 State machine agent run
Agent run lifecycle
queued
  -> preparing_workspace
  -> running_agent
  -> collecting_diff
  -> running_tests
  -> auto_commit
  -> auto_push_or_pr
  -> completed

Failure states:
  -> failed_agent
  -> failed_tests
  -> blocked_by_policy
  -> cancelled
  -> timed_out
6.4 Kenapa Rust cocok
•	Concurrency: banyak WebSocket dan stream event bisa ditangani efisien dengan Tokio.
•	Safety: type system membantu mengurangi bug pada state machine dan policy engine.
•	Performance: log streaming, SSE parsing, dan fanout realtime ringan di CPU/RAM.
•	Single binary deployment: mudah dipasang di VPS dan container.
7. OpenClaw integration design
7.1 Peran OpenClaw
OpenClaw dipakai sebagai agent runtime: routing agent, session routing, dan tool orchestration. Backend Rust tetap menjadi product backend dan security/policy layer. OpenClaw Gateway tidak diekspos ke Android app atau internet publik.
•	Backend memanggil OpenClaw endpoint /v1/responses dengan stream=true.
•	Backend mengirim x-openclaw-agent-id berdasarkan project.
•	Backend mengirim x-openclaw-session-key berdasarkan coding session.
•	Backend parse SSE dari OpenClaw lalu broadcast event ke Android via WebSocket.
•	OpenClaw menjalankan tool execution di sandbox. Dokumentasi OpenClaw menyebut sandbox bisa mencakup exec/read/write/edit/apply_patch/process. Lihat [R2].
7.2 OpenClaw call pattern
HTTP request ke OpenClaw
POST http://127.0.0.1:18789/v1/responses
Authorization: Bearer <OPENCLAW_GATEWAY_TOKEN>
Content-Type: application/json
x-openclaw-agent-id: project-main-api
x-openclaw-session-key: team_<team_id>:project_<project_id>:session_<session_id>

{
  "model": "openclaw",
  "stream": true,
  "user": "user_<user_id>",
  "instructions": "<backend-generated agent instructions>",
  "input": "Tambahin endpoint health check dan test-nya"
}
7.3 Session key strategy
•	Session key tidak dianggap auth. Auth tetap JWT backend.
•	Format session key dibuat deterministic agar conversation context stabil.
•	Contoh: team_{teamId}:project_{projectId}:session_{sessionId}.
•	Jika session di-archive, backend tidak lagi meneruskan event ke session tersebut.
7.4 Gateway placement
Opsi	Cocok untuk	Keputusan
Host service	VPS tunggal, dev cepat	Recommended untuk awal, port bind localhost.
Docker container	Isolasi lebih rapi	Boleh, tapi volume mapping dan sandbox path harus hati-hati.
Separate runner VPS	Team mulai aktif	Recommended fase 2 untuk blast radius lebih kecil.

Catatan dari docs
OpenClaw Docker/sandboxing punya perhatian khusus soal host path dan volume mapping: Docker daemon mengevaluasi path relatif ke Host OS namespace, bukan namespace Gateway container. Karena itu, workspace path harus konsisten antara host dan container. Lihat [R2].
8. Auto Mode tanpa approval UI
8.1 Prinsip Auto Mode
Karena app tidak punya approval UI, keamanan harus dipindah ke desain sistem: sandbox disposable, branch/worktree per run, no production secrets, hard deny policy, test gate, audit log, timeout, dan rollback. Sistem tidak bertanya ke user untuk setiap tindakan; sistem otomatis menjalankan yang aman dan otomatis menghentikan yang melanggar policy.
8.2 Mode auto yang disarankan
Mode	Perilaku	Cocok untuk
auto_safe	Boleh baca/edit repo, run test/lint/build. Tidak install dependency baru. Tidak push. Hasil akhir diff saja.	MVP paling aman.
auto_trusted	Boleh install dependency dari registry allowlist, commit, push branch, create PR otomatis jika test pass.	Rekomendasi utama untuk internal team kecil.
auto_full	Boleh lebih banyak command termasuk deploy/staging jika policy mengizinkan.	Tidak direkomendasikan di fase awal.

8.3 Default policy rekomendasi
Kategori	Auto Safe	Auto Trusted	Catatan
Read/list/search files	Allow	Allow	Workspace only.
Edit files	Allow	Allow	Hanya worktree sementara.
Run tests/lint/build	Allow	Allow	Timeout wajib.
Install dependency	Deny	Allow with package registry allowlist	Network dibuka terbatas.
Git commit	Deny	Allow	Commit di branch AI.
Git push branch	Deny	Allow	Tidak push main.
Create PR	Deny	Allow	Via GitHub/GitLab App token restricted.
Merge PR	Deny	Deny by default	Bisa enabled setelah CI dan branch protection matang.
Deploy staging	Deny	Optional allow	Fase awal tetap deny.
Deploy production	Deny	Deny	Jangan auto di fase awal.
Read .env/secrets	Deny	Deny	Gunakan dummy test env.
Host/system command	Deny	Deny	Sandbox only.

8.4 Cara auto tanpa granular approval tetap aman
•	Setiap run menggunakan branch/worktree disposable. Jika rusak, hapus dan ulangi dari main.
•	Main branch tidak pernah menjadi working directory agent.
•	Sandbox berjalan non-root dan tidak punya docker.sock.
•	Network sandbox default off. Untuk dependency install, gunakan network window terbatas atau registry allowlist.
•	Secrets production tidak ada di workspace. Test memakai dummy env.
•	Backend hanya auto-push branch AI, bukan merge ke main.
•	Jika test gagal setelah retry limit, run selesai sebagai failed_tests dan app menampilkan ringkasan. Tidak ada approval prompt.
•	Jika policy melarang tindakan, run selesai sebagai blocked_by_policy atau agent diminta mencoba alternatif.
8.5 Auto policy YAML
Contoh project policy
auto_mode: auto_trusted
limits:
  max_run_minutes: 30
  max_command_minutes: 5
  max_changed_files: 30
  max_diff_lines: 3000
  max_retries: 3
workspace:
  require_worktree: true
  allow_main_branch_write: false
  cleanup_after_days: 7
network:
  default: none
  dependency_install_window: true
  allowed_hosts:
    - registry.npmjs.org
    - pypi.org
    - files.pythonhosted.org
    - proxy.golang.org
    - crates.io
commands:
  allow_patterns:
    - "^git (status|diff|log|show|branch|checkout|switch|add|commit)"
    - "^(npm|pnpm|yarn) (test|run test|run lint|run build)"
    - "^pytest"
    - "^go test ./..."
    - "^cargo test"
  deny_patterns:
    - "sudo"
    - "su "
    - "systemctl"
    - "docker"
    - "kubectl"
    - "rm -rf /"
    - "chmod 777"
    - "cat .env"
    - "printenv"
git:
  auto_commit: true
  auto_push_branch: true
  auto_create_pr: true
  auto_merge: false
8.6 Trade-off tanpa approval
Keuntungan	Risiko	Mitigasi
UX cepat dan hands-free	Agent bisa salah arah	Small diffs, retry limit, branch isolation.
Tidak perlu user monitor terus	Dependency berisiko	Allowlist registry, lockfile diff review via PR.
Cocok mobile	Command destruktif di workspace	Disposable worktree, backup repo remote.
Automation tinggi	Prompt injection dari repo/docs	No secrets, network off, deny external fetch default.

9. Workflow coding end-to-end
9.1 Workflow utama dari Android
1. User membuka Android app dan login.
2. App mengambil daftar project via GET /api/projects.
3. User membuka project dan session.
4. User mengirim prompt coding via POST /api/sessions/{id}/agent-runs.
5. Backend validasi JWT, role, project permission, quota, dan policy.
6. Backend membuat agent_run dengan status queued.
7. Worker membuat branch/worktree disposable untuk run.
8. Worker memanggil OpenClaw /v1/responses stream=true.
9. Backend parse SSE dan broadcast event ke Android via WebSocket.
10. Agent membaca repo, edit file, menjalankan test/lint/build sesuai policy.
11. Backend mengumpulkan git diff dan test result.
12. Jika policy auto_trusted dan test pass, backend commit, push branch, create PR.
13. Run selesai completed. Android menampilkan summary, file changes, PR link, dan logs.
9.2 Workflow jika test gagal
Test failure flow
agent edit -> run test -> test failed
  -> agent receives failure output
  -> retry fix up to max_retries
  -> if pass: continue auto_commit
  -> if still fail: status failed_tests
  -> app shows summary + diff + failing command
9.3 Workflow jika tindakan diblok policy
Policy blocked flow
agent tries risky operation
  -> sandbox/policy blocks or command fails
  -> backend records policy event
  -> agent attempts safe alternative
  -> if impossible: status blocked_by_policy
  -> app shows what was blocked and why
9.4 Workflow dependency install
•	auto_safe: dependency install ditolak. Agent harus solusi tanpa dependency baru atau run berakhir blocked_by_policy.
•	auto_trusted: dependency install boleh jika registry/host masuk allowlist dan package manager sesuai project instruction.
•	Setelah install, lockfile wajib masuk diff. Backend mencatat package dan versi di audit log.
•	Network window ditutup lagi setelah install selesai.
9.5 Workflow PR otomatis
1. Backend menjalankan git diff dan git status.
2. Backend memastikan tidak ada file denylist berubah: .env, credentials, binary besar, generated secrets.
3. Backend menjalankan final test gate.
4. Backend commit dengan format: feat/fix/chore(scope): AI run summary.
5. Backend push ke branch ai/{date}-{run_id}-{slug}.
6. Backend create PR memakai GitHub/GitLab App token restricted.
7. Android app menerima event pr.created dengan URL PR.
10. Multi-user dan concurrency
10.1 Role model
Role	Boleh melakukan
Owner	Manage team, project, policies, Git provider integration, user roles.
Admin	Manage project, view all runs, edit policy non-critical.
Developer	Create sessions, start agent runs, view logs/diff, push branch via auto policy.
Viewer	View sessions/logs/diff only.

10.2 Concurrency model MVP
•	Banyak user boleh membaca project/session yang sama.
•	Satu project boleh punya banyak read-only sessions.
•	Untuk write/edit agent run, gunakan project write lock pada MVP.
•	Jika lock aktif, run baru masuk queue dan app melihat status queued.
•	Setelah worktree per run stabil, concurrency bisa dinaikkan menjadi beberapa run per project.
Project write lock
Redis lock key:
  project:{project_id}:write-lock

Value:
  agent_run_id

TTL:
  30 minutes, renewed every 60 seconds while running
10.3 Concurrency advanced
•	Setiap agent run punya git worktree sendiri.
•	Branch per run membuat perubahan tidak saling menimpa.
•	Konflik baru muncul saat merge PR, bukan saat agent coding.
•	Backend bisa menjalankan maksimal N run paralel per project sesuai resource VPS.
11. Git, workspace, dan sandbox lifecycle
11.1 Directory layout
VPS directory layout
/srv/ai-platform/
  app/
    backend/
  data/
    postgres/
    redis/
  workspaces/
    team_acme/
      project_main_api/        # canonical clone
  worktrees/
    team_acme/
      project_main_api/
        run_20260523_abc123/   # disposable per run
  logs/
    backend/
    agent-runs/
  backups/
11.2 Branch dan worktree
Worktree creation
# prepare canonical repo
git -C /srv/ai-platform/workspaces/team_acme/project_main_api fetch origin
git -C /srv/ai-platform/workspaces/team_acme/project_main_api switch main
git -C /srv/ai-platform/workspaces/team_acme/project_main_api pull --ff-only origin main

# create disposable worktree
git -C /srv/ai-platform/workspaces/team_acme/project_main_api worktree add   /srv/ai-platform/worktrees/team_a
cme/project_main_api/run_abc123   -b ai/2026-05-23-abc123-health-check
11.3 Cleanup
•	Completed run: keep worktree 7 hari untuk debug, lalu hapus otomatis.
•	Failed run: keep 3 hari atau sesuai policy.
•	Cancelled/timed out: hapus container segera, simpan logs dan diff jika ada.
•	PR merged: worktree dan branch lokal bisa dihapus.
11.4 Sandbox lifecycle
Tahap	Aksi
Create	OpenClaw/sandbox backend membuat container per session/run.
Mount	Mount worktree run sebagai workspace rw, tanpa host home/secrets.
Execute	Agent tool execution berjalan non-root, limited CPU/RAM/disk.
Network	Default none; temporary allowlist untuk dependency install jika auto_trusted.
Stop	Container dihentikan setelah run selesai/timed out.
Destroy	Container dihapus; output penting sudah disimpan di audit/log.

12. API design dan WebSocket event protocol
12.1 REST endpoints
REST API
Auth:
  POST /api/auth/login
  POST /api/auth/refresh
  POST /api/auth/logout
  GET  /api/me

Projects:
  GET  /api/projects
  POST /api/projects
  GET  /api/projects/{project_id}
  PATCH /api/projects/{project_id}
  GET  /api/projects/{project_id}/files
  GET  /api/projects/{project_id}/files/content?path=src/main.rs

Sessions:
  POST /api/projects/{project_id}/sessions
  GET  /api/projects/{project_id}/sessions
  GET  /api/sessions/{session_id}
  GET  /api/sessions/{session_id}/messages

Agent Runs:
  POST /api/sessions/{session_id}/agent-runs
  GET  /api/agent-runs/{run_id}
  POST /api/agent-runs/{run_id}/cancel
  GET  /api/agent-runs/{run_id}/diff
  GET  /api/agent-runs/{run_id}/events?after_seq=123

Policy/Admin:
  GET  /api/projects/{project_id}/policy
  PATCH /api/projects/{project_id}/policy

Audit:
  GET  /api/projects/{project_id}/audit-logs
12.2 WebSocket protocol
WebSocket subscribe
WS /ws
Authorization: Bearer <jwt>

Client -> Server:
{
  "type": "subscribe.session",
  "session_id": "sess_123",
  "after_seq": 98
}

Server -> Client:
{
  "type": "agent_run.started",
  "seq": 99,
  "session_id": "sess_123",
  "run_id": "run_123"
}
12.3 Event types
Event	Keterangan
message.created	User/assistant/tool message tersimpan.
agent_run.queued	Run masuk queue.
agent_run.started	Worker mulai menjalankan run.
assistant.delta	Token/text stream dari agent.
tool.started	Tool/command dimulai.
terminal.output	stdout/stderr stream.
policy.blocked	Tindakan diblok auto policy.
file.changed	File berubah atau diff diperbarui.
tests.started	Test/lint/build dimulai.
tests.finished	Test/lint/build selesai.
git.committed	Commit berhasil dibuat.
pr.created	PR/merge request dibuat.
agent_run.completed	Run selesai sukses.
agent_run.failed	Run gagal.

12.4 Example run event
PR created event
{
  "type": "pr.created",
  "seq": 245,
  "session_id": "sess_123",
  "run_id": "run_123",
  "project_id": "proj_123",
  "url": "https://github.com/company/repo/pull/42",
  "branch": "ai/2026-05-23-run123-health-check",
  "summary": {
    "files_changed": 3,
    "insertions": 84,
    "deletions": 12,
    "tests": "passed"
  }
}
13. Database schema
13.1 Core tables
Table	Fungsi
users	User internal team.
teams	Organisasi/team.
team_members	Role user di team.
projects	Repo/project config dan OpenClaw agent mapping.
project_members	Role user di project.
coding_sessions	Conversation/session per project.
messages	Chat messages dan tool messages.
agent_runs	Satu pekerjaan agent.
run_events	Event stream durable dengan seq.
tool_events	Ringkasan tool/command.
file_changes	Diff per file.
project_policies	Auto-mode policy per project.
audit_logs	Audit security dan operations.

13.2 Agent run table
SQL: agent_runs
CREATE TABLE agent_runs (
  id UUID PRIMARY KEY,
  session_id UUID NOT NULL REFERENCES coding_sessions(id),
  project_id UUID NOT NULL REFERENCES projects(id),
  user_id UUID NOT NULL REFERENCES users(id),

  prompt TEXT NOT NULL,
  status TEXT NOT NULL,
  auto_mode TEXT NOT NULL DEFAULT 'auto_trusted',

  openclaw_agent_id TEXT NOT NULL,
  openclaw_session_key TEXT NOT NULL,

  branch_name TEXT,
  worktree_path TEXT,
  commit_sha TEXT,
  pr_url TEXT,

  started_at TIMESTAMPTZ,
  finished_at TIMESTAMPTZ,
  error_message TEXT,

  usage JSONB NOT NULL DEFAULT '{}',
  metadata JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
13.3 Run events table
SQL: run_events
CREATE TABLE run_events (
  id UUID PRIMARY KEY,
  run_id UUID NOT NULL REFERENCES agent_runs(id),
  session_id UUID NOT NULL REFERENCES coding_sessions(id),
  seq BIGINT NOT NULL,
  event_type TEXT NOT NULL,
  payload JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(session_id, seq)
);
13.4 Project policy table
SQL: project_policies
CREATE TABLE project_policies (
  id UUID PRIMARY KEY,
  project_id UUID NOT NULL REFERENCES projects(id),
  auto_mode TEXT NOT NULL CHECK (auto_mode IN ('auto_safe', 'auto_trusted', 'auto_full')),
  policy JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(project_id)
);
14. Deployment VPS dan Docker topology
14.1 Topology fase awal
Single VPS topology
Single VPS:
  caddy/nginx        public :80/:443
  rust-backend       private container/service
  postgres           private
  redis              private
  openclaw-gateway   127.0.0.1:18789 or private docker network
  sandbox containers private, no public ports
  workspaces         /srv/ai-platform/workspaces
  worktrees          /srv/ai-platform/worktrees
14.2 VPS sizing
Tier	Spec	Cocok untuk
Minimum	4 vCPU, 8 GB RAM, 100 GB NVMe	1-3 user aktif, project kecil.
Recommended	8 vCPU, 16-32 GB RAM, 200 GB NVMe	Internal team kecil, beberapa run/hari.
Scale-out	App VPS + Runner VPS terpisah	Saat agent run sering atau repo besar.

14.3 Docker compose baseline
docker-compose.yml baseline
services:
  caddy:
    image: caddy:2
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy_data:/data
    depends_on:
      - backend

  backend:
    build: ./backend
    environment:
      DATABASE_URL: postgres://postgres:postgres@postgres:5432/aicode
      REDIS_URL: redis://redis:6379
      OPENCLAW_BASE_URL: http://host.docker.internal:18789
      OPENCLAW_GATEWAY_TOKEN: ${OPENCLAW_GATEWAY_TOKEN}
      JWT_SECRET: ${JWT_SECRET}
    extra_hosts:
      - "host.docker.internal:host-gateway"
    volumes:
      - /srv/ai-platform/workspaces:/srv/ai-platform/workspaces
      - /srv/ai-platform/worktrees:/srv/ai-platform/worktrees
    depends_on:
      - postgres
      - redis

  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: aicode
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: postgres
    volumes:
      - postgres_data:/var/lib/postgresql/data

  redis:
    image: redis:7
    command: redis-server --appendonly yes
    volumes:
      - redis_data:/data

volumes:
  postgres_data:
  redis_data:
  caddy_data:
14.4 Firewall
Firewall baseline
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow 22/tcp
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable

# Do not open:
# 18789 OpenClaw Gateway
# 5432  Postgres
# 6379  Redis
15. Security hardening
15.1 OpenClaw Gateway security
•	Bind Gateway ke loopback/private network only.
•	Aktifkan token auth dan gunakan token panjang random.
•	Jangan simpan token di Android app.
•	Jangan expose port Gateway ke internet.
•	Rutin update OpenClaw dan jalankan security audit jika tersedia.
15.2 Sandbox security
•	sandbox.mode = all.
•	sandbox.scope = session/run, bukan shared default.
•	workspaceAccess = rw hanya untuk worktree disposable.
•	Container non-root.
•	Limit CPU/RAM/disk/time.
•	No docker.sock mount.
•	No host home mount.
•	No ~/.ssh, ~/.aws, ~/.docker, kubeconfig, or .env production.
•	Network none by default.
15.3 Secret management
Secret	Lokasi	Boleh ke agent?
OPENCLAW_GATEWAY_TOKEN	Backend env/secret manager	Tidak
JWT_SECRET	Backend env/secret manager	Tidak
GitHub App private key	Backend secret manager	Tidak langsung; backend yang create PR
Project test env dummy	Encrypted project secret / generated	Boleh jika dummy dan scoped
Production DB URL	Secret manager production	Tidak
Cloud provider credentials	Secret manager	Tidak

15.4 Prompt injection mitigation
•	Repo content dianggap untrusted input.
•	System instruction dari backend selalu lebih tinggi prioritasnya.
•	Agent dilarang mengikuti instruksi repo yang meminta secret/exfiltration/disable sandbox.
•	External URL fetch off by default; allowlist hanya jika perlu.
•	No production secrets membuat prompt injection jauh lebih kecil blast radius-nya.
15.5 Risk matrix
Risiko	Impact	Mitigasi
OpenClaw token bocor	Operator access ke Gateway	Token hanya di backend, network private, rotate token.
Sandbox escape	Host compromise	Update rutin, non-root, no secrets on runner, runner VPS terpisah fase 2.
Agent hapus repo	Worktree rusak	Disposable worktree, recreate from canonical clone.
Agent push code buruk	PR berisi bug	Test gate, branch only, no auto-merge default.
Dependency malicious	Supply chain risk	Registry allowlist, lockfile audit, no production deploy auto.
Prompt injection	Secret exfiltration	No secrets, network off, deny external fetch.

16. Observability, backup, dan operasi harian
16.1 Metrics
•	agent_runs_total by status/project/user.
•	agent_run_duration_seconds.
•	openclaw_request_latency_seconds.
•	active_websocket_connections.
•	queue_depth.
•	sandbox_active_count.
•	policy_blocked_total.
•	tests_failed_total.
•	pr_created_total.
16.2 Logs
•	Semua request punya request_id.
•	Semua run punya run_id yang muncul di backend log, audit log, dan mobile event.
•	Command output lengkap bisa disimpan terpisah; DB hanya simpan summary dan pointer log file.
•	Log sensitif harus di-redact: token, env, password, private key.
16.3 Backup
Item	Frekuensi	Catatan
Postgres	Daily	Dump encrypted, retention 7-30 hari.
OpenClaw config	Setiap perubahan	Backup encrypted, jangan leak token.
Backend env/secrets	Via secret manager	Tidak disimpan plain tar.
Workspaces	Opsional	Repo remote adalah source of truth; backup uncommitted run logs jika perlu.
Audit logs	Ikut Postgres	Jangan mudah dihapus manual.

16.4 Daily operations
•	Cek health backend, Postgres, Redis, OpenClaw.
•	Cek queue depth dan failed runs.
•	Cek disk usage worktrees/logs.
•	Cleanup run lama otomatis.
•	Update OpenClaw dan sandbox image secara periodik setelah test di staging.
17. Roadmap implementasi
Phase	Durasi	Deliverables	Done jika
0. Infra	2-3 hari	VPS, domain, Docker, Caddy, Postgres, Redis, OpenClaw private	Backend bisa call /v1/responses.
1. Rust backend core	4-6 hari	Auth, projects, sessions, WS, DB migrations	Android dummy client bisa login dan subscribe.
2. Android MVP	4-6 hari	Login, project list, chat, run timeline	Prompt bisa dikirim dan stream tampil.
3. Agent run	5-7 hari	Queue, worktree, OpenClaw stream parser, run lifecycle	Agent bisa jawab dan edit repo.
4. Auto mode	5-7 hari	Policy config, test gate, auto commit/push/PR	Run sukses otomatis create PR.
5. Hardening	5-10 hari	Sandbox polish, logs, backup, metrics, cleanup	Internal beta aman untuk 2-5 user.

17.1 Prioritas build
1. Bangun backend Rust minimal dengan health, auth, project, session, WebSocket.
2. Buat Android app minimal: login, project, chat, event stream.
3. Integrasi OpenClaw /v1/responses streaming dari backend.
4. Tambahkan workspace/worktree dan git diff.
5. Aktifkan sandbox dan auto policy.
6. Tambahkan auto commit/push/PR.
7. Baru polish UI diff viewer, logs, metrics, dan admin policy screen.
18. Checklist siap pakai
18.1 MVP checklist
•	[ ] Domain dan HTTPS aktif.
•	[ ] OpenClaw Gateway tidak bisa diakses publik.
•	[ ] Backend Rust bisa call OpenClaw /v1/responses stream=true.
•	[ ] Android app bisa login dan connect WebSocket.
•	[ ] Session event punya seq dan bisa resume.
•	[ ] Project bisa clone/fetch repo.
•	[ ] Agent run membuat branch/worktree disposable.
•	[ ] Agent bisa edit file di worktree.
•	[ ] Backend bisa collect diff.
•	[ ] Test gate berjalan.
•	[ ] Auto commit/push/PR sesuai policy.
•	[ ] Audit log mencatat user, prompt, run, command summary, diff summary.
18.2 Security checklist
•	[ ] Android tidak menyimpan OpenClaw token.
•	[ ] Gateway bind loopback/private only.
•	[ ] Postgres/Redis private only.
•	[ ] Sandbox mode all/session.
•	[ ] No docker.sock mount.
•	[ ] No host home mount.
•	[ ] No production secrets in workspace.
•	[ ] Network sandbox none by default.
•	[ ] Main branch write disabled.
•	[ ] Auto merge disabled by default.
•	[ ] Logs redact token/env/secrets.
18.3 Android release checklist
•	[ ] Token stored securely.
•	[ ] WebSocket reconnect tested on bad network.
•	[ ] App can resume events using after_seq.
•	[ ] Large terminal output does not freeze UI.
•	[ ] Diff viewer handles large files gracefully.
•	[ ] Logout clears local session cache/token.
19. Appendix: config dan snippet
19.1 OpenClaw config baseline
openclaw.json baseline konsep
{
  "gateway": {
    "mode": "local",
    "bind": "loopback",
    "port": 18789,
    "auth": {
      "mode": "token",
      "token": "REPLACE_WITH_LONG_RANDOM_TOKEN"
    },
    "http": {
      "endpoints": {
        "responses": {
          "enabled": true,
          "files": { "allowUrl": false },
          "images": { "allowUrl": false }
        }
      }
    }
  },
  "agents": {
    "defaults": {
      "sandbox": {
        "mode": "all",
        "backend": "docker",
        "scope": "session",
        "workspaceAccess": "rw",
        "docker": {
          "image": "openclaw-sandbox-common:bookworm-slim",
          "network": "none"
        }
      }
    }
  },
  "tools": {
    "elevated": { "enabled": false }
  }
}
19.2 Rust OpenClaw client pseudo-code
Rust client pseudo-code
pub async fn run_openclaw_stream(input: OpenClawRunInput) -> anyhow::Result<()> {
    let res = reqwest::Client::new()
        .post(format!("{}/v1/responses", input.base_url))
        .bearer_auth(input.gateway_token)
        .header("x-openclaw-agent-id", input.agent_id)
        .header("x-openclaw-session-key", input.session_key)
        .json(&serde_json::json!({
            "model": "openclaw",
            "stream": true,
            "user": input.user_id,
            "instructions": input.instructions,
            "input": input.prompt
        }))
        .send()
        .await?;

    if !res.status().is_success() {
        anyhow::bail!("OpenClaw failed: {}", res.status());
    }

    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        // parse SSE chunk -> normalize event -> save run_events -> WS fanout
    }

    Ok(())
}
19.3 Agent system instruction template
System instruction
You are the internal coding agent for this team.

Project:
- Name: {{project_name}}
- Repository: {{repo_slug}}
- Branch/worktree: {{branch_name}}

Rules:
- Work only inside the assigned workspace.
- Never access .env, SSH keys, cloud credentials, or host files.
- Never edit main branch directly.
- Prefer small, reviewable diffs.
- Run relevant tests after changes.
- Do not deploy production.
- If a command is blocked, choose a safe alternative.
- At the end, summarize files changed, commands run, tests, and risks.
19.4 Project AGENTS.md template
AGENTS.md template
# Agent Instructions

## Stack
- Backend: Rust Axum
- Mobile: Kotlin Jetpack Compose
- Database: PostgreSQL

## Commands
- cargo test
- cargo fmt --check
- cargo clippy -- -D warnings

## Rules
- Do not edit .env or secrets.
- Do not modify production deployment configs without project policy.
- Use small commits.
- Add or update tests for behavior changes.
20. Referensi teknis
Sumber berikut dipakai untuk bagian OpenClaw dan deployment. Periksa ulang dokumentasi resmi saat implementasi karena API/config bisa berubah. Diakses: 23 Mei 2026.
•	[R1] OpenClaw - OpenResponses API: https://docs.openclaw.ai/gateway/openresponses-http-api
•	[R2] OpenClaw - Sandboxing: https://docs.openclaw.ai/gateway/sandboxing
•	[R3] OpenClaw - Remote access: https://docs.openclaw.ai/gateway/remote
•	[R4] OpenClaw - Docker install: https://docs.openclaw.ai/install/docker
•	[R5] OpenClaw - Gateway overview: https://docs.openclaw.ai/id/gateway
Penutup
Blueprint ini sudah disesuaikan untuk internal team kecil: Android Kotlin sebagai client, Rust sebagai backend utama, OpenClaw sebagai agent runtime private, dan Auto Mode tanpa approval UI. Kunci utamanya adalah bukan membiarkan agent bebas, tetapi membuat lingkungan agent disposable dan sempit sehingga automation tetap cepat namun blast radius tetap kecil.
