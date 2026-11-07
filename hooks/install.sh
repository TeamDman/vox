#!/usr/bin/env bash
set -euo pipefail

# This fork deliberately does not install Git hooks. Keep this entry point so
# existing desktop and cloud setup commands also disable previously copied hooks.
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
git_dir="$(git -C "$repo_root" rev-parse --path-format=absolute --git-common-dir)"
disabled_hooks_dir="$git_dir/hooks-disabled"
mkdir -p "$disabled_hooks_dir"
git -C "$repo_root" config --local core.hooksPath "$disabled_hooks_dir"
if [[ "$(git -C "$repo_root" config --local --bool --get extensions.worktreeConfig || true)" == true ]]; then
  git -C "$repo_root" config --worktree core.hooksPath "$disabled_hooks_dir"
fi
printf '%s\n' 'Git hooks are disabled for this checkout.'
