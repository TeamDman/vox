#!/usr/bin/env bash
set -euo pipefail

# Compatibility entry point for existing cloud and desktop setup commands.
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$script_dir/../hooks/install.sh" "$@"
