#!/usr/bin/env bash
# Prints `true` when the event changed a file matching one of the patterns
# given (bash patterns, where `*` also matches `/`), `false` otherwise. With
# `--or-removed` first, a file deleted or renamed anywhere counts too.
#
# Compared with: the pull request's base, the commit before a push, and main
# for a manual run. With no base to compare with, `true`: a run too many is
# better than a check skipped.
set -euo pipefail

removed=false
if [ "${1:-}" = --or-removed ]; then
  removed=true
  shift
fi

case $GITHUB_EVENT_NAME in
  pull_request) range="$BASE_SHA...HEAD" ;;
  push)
    if [ -z "$BEFORE" ] || [[ $BEFORE =~ ^0+$ ]]; then
      echo true
      exit
    fi
    range="$BEFORE..HEAD"
    ;;
  *) range="origin/main...HEAD" ;;
esac

if ! files=$(git diff --name-only "$range" 2>/dev/null); then
  echo true
  exit
fi
if $removed && [ -n "$(git diff --name-only --diff-filter=DR "$range")" ]; then
  echo true
  exit
fi
while IFS= read -r file; do
  for pattern in "$@"; do
    # shellcheck disable=SC2053 # the pattern is meant to match
    if [[ $file == $pattern ]]; then
      echo true
      exit
    fi
  done
done <<<"$files"
echo false
