#!/bin/bash
# Setup skills and MCP context7 for dealtech-code platform
# Run this after docker-compose is set up

set -e

echo "=== Setting up skills ==="
cd /srv/ai-platform
if [ ! -d "skills" ]; then
    git clone https://github.com/Deal-Tech/skills.git skills
    echo "✅ Skills cloned"
else
    cd skills && git pull && cd ..
    echo "✅ Skills updated"
fi

echo ""
echo "=== Setting up MCP Context7 for OpenClaw ==="
if command -v openclaw &> /dev/null; then
    openclaw mcp set context7 --command "npx" --args "-y" "@upstash/context7-mcp" 2>/dev/null || {
        echo "Adding context7 to openclaw config manually..."
        # Will be picked up on next gateway restart
        node -e "
const fs = require('fs');
const p = require('os').homedir() + '/.openclaw/openclaw.json';
if (fs.existsSync(p)) {
  const c = JSON.parse(fs.readFileSync(p, 'utf8'));
  if (!c.mcp) c.mcp = {};
  if (!c.mcp.servers) c.mcp.servers = {};
  c.mcp.servers.context7 = { command: 'npx', args: ['-y', '@upstash/context7-mcp'] };
  fs.writeFileSync(p, JSON.stringify(c, null, 2));
  console.log('✅ MCP context7 added to openclaw config');
}
"
    }
else
    echo "⚠️  OpenClaw not installed. Install openclaw first, then re-run this script."
fi

echo ""
echo "=== Done ==="
echo "Restart services: docker compose up -d backend"
echo "Restart OpenClaw: systemctl --user restart openclaw-gateway"
