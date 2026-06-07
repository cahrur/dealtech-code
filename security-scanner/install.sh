#!/bin/bash
# Security Scanner Installer
# Installs Nuclei and sets up the scanning environment

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NUCLEI_VERSION="3.3.7"
NUCLEI_URL="https://github.com/projectdiscovery/nuclei/releases/download/v${NUCLEI_VERSION}/nuclei_${NUCLEI_VERSION}_linux_amd64.zip"

echo "🔧 Security Scanner Installer"
echo "=============================="

# Check if running as root or with sudo
if [ "$EUID" -ne 0 ] && ! command -v sudo &>/dev/null; then
    echo "❌ Need root or sudo access"
    exit 1
fi

SUDO=""
if [ "$EUID" -ne 0 ]; then
    SUDO="sudo"
fi

# Install dependencies
echo ""
echo "📦 Installing dependencies..."
$SUDO apt-get update -qq
$SUDO apt-get install -y -qq unzip curl jq > /dev/null 2>&1
echo "✅ Dependencies installed"

# Install Nuclei
echo ""
echo "🧬 Installing Nuclei v${NUCLEI_VERSION}..."
if command -v nuclei &>/dev/null; then
    CURRENT_VER=$(nuclei -version 2>&1 | grep -oP 'v[\d.]+' | head -1)
    echo "   Already installed: ${CURRENT_VER}"
    echo "   Updating to v${NUCLEI_VERSION}..."
fi

TEMP_DIR=$(mktemp -d)
curl -sL "$NUCLEI_URL" -o "${TEMP_DIR}/nuclei.zip"
unzip -q "${TEMP_DIR}/nuclei.zip" -d "${TEMP_DIR}"
$SUDO mv "${TEMP_DIR}/nuclei" /usr/local/bin/nuclei
$SUDO chmod +x /usr/local/bin/nuclei
rm -rf "${TEMP_DIR}"
echo "✅ Nuclei installed: $(nuclei -version 2>&1 | head -1)"

# Update Nuclei templates
echo ""
echo "📋 Downloading Nuclei templates (this may take a minute)..."
nuclei -update-templates -silent 2>/dev/null || nuclei -ut -silent 2>/dev/null || true
echo "✅ Templates updated"

# Setup directories
echo ""
echo "📁 Setting up directories..."
mkdir -p "${SCRIPT_DIR}/results"
mkdir -p "${SCRIPT_DIR}/templates"
mkdir -p "${SCRIPT_DIR}/lib"
echo "✅ Directories ready"

# Make scripts executable
chmod +x "${SCRIPT_DIR}/scan.sh" 2>/dev/null || true
chmod +x "${SCRIPT_DIR}/lib/"*.sh 2>/dev/null || true

# Create symlink for easy access
$SUDO ln -sf "${SCRIPT_DIR}/scan.sh" /usr/local/bin/secscan 2>/dev/null || true

echo ""
echo "=============================="
echo "✅ Installation complete!"
echo ""
echo "Usage:"
echo "  secscan <url>              # Quick scan"
echo "  secscan <url> --full       # Full scan"
echo "  secscan <url> --auth <token>  # Authenticated scan"
echo ""
echo "Or directly: ${SCRIPT_DIR}/scan.sh <url>"
