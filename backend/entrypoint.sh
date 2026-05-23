#!/bin/bash
set -e

# Configure git credentials if GITHUB_TOKEN is set
if [ -n "$GITHUB_TOKEN" ]; then
    git config --global credential.helper store
    echo "https://x-access-token:${GITHUB_TOKEN}@github.com" > ~/.git-credentials
    chmod 600 ~/.git-credentials
    git config --global user.email "agent@ai-platform"
    git config --global user.name "AI Agent"
fi

exec ./backend
