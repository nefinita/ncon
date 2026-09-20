#!/bin/sh
# Upstream bcon apt repository installer (kept for reference).
# NOTE: this installs the upstream project; ncon is not published as a Debian package yet.
# Usage: curl -fsSL https://sanohiro.github.io/bcon/install.sh | sudo sh

set -e

# Add GPG key
curl -fsSL https://sanohiro.github.io/bcon/bcon.gpg | gpg --dearmor -o /usr/share/keyrings/ncon.gpg

# Add repository
echo "deb [signed-by=/usr/share/keyrings/ncon.gpg] https://sanohiro.github.io/bcon stable main" > /etc/apt/sources.list.d/ncon.list

# Update package list
apt update

echo "Done! Run 'apt install bcon' to install."
