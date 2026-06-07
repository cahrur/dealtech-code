# Security Scanner

Tool internal tim untuk security testing website/API menggunakan Nuclei.

## Install

```bash
cd security-scanner
chmod +x install.sh
./install.sh
```

## Usage

```bash
# Quick scan (2-5 menit)
secscan https://target.com

# Full scan (15-30 menit)
secscan https://target.com --full

# Scan dengan authentication
secscan https://target.com --token eyJhbGciOiJIUzI1NiJ9...
secscan https://target.com --cookie "session=abc123; csrf=xyz"

# Scan spesifik
secscan https://target.com --cves              # Known CVEs only
secscan https://target.com --misconfig         # Misconfigurations
secscan https://target.com --exposure          # Exposed files/panels
secscan https://target.com --recon             # Recon/info gathering

# Custom severity filter
secscan https://target.com --severity critical,high

# Output format
secscan https://target.com --json
secscan https://target.com --markdown
```

## Scan Modes

| Mode | Waktu | Coverage |
|------|-------|----------|
| quick | 2-5 min | CVE, RCE, SQLi, XSS, LFI, SSRF |
| full | 15-30 min | Semua templates |
| recon | 1-2 min | Tech detect, DNS, WAF |
| cves | 5-10 min | Known CVEs |
| misconfig | 5-10 min | Misconfigurations |
| exposure | 3-5 min | Exposed files, panels |

## Authentication

**JANGAN kirim password.** Gunakan session token/cookie:

1. Login ke target website di browser
2. Buka DevTools → Application → Cookies
3. Copy session cookie value
4. Pakai: `secscan https://target.com --cookie "session_name=value"`

Atau kalau API dengan Bearer token:
```bash
secscan https://api.target.com --token YOUR_JWT_TOKEN
```

## Results

Hasil scan disimpan di `results/` directory:
- `.jsonl` — raw findings (machine-readable)
- `.md` — markdown report (kalau pakai --markdown)

## Limitations

Automated scanner TIDAK bisa detect:
- Business logic bugs
- IDOR (butuh 2 user context)
- Race conditions
- Complex auth bypass
- Chained vulnerabilities

Untuk itu, kombinasikan dengan manual testing / AI-assisted analysis.
