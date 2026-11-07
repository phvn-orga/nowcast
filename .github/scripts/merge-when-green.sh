#!/usr/bin/env bash
# Squash-merges pull request $1 once its required checks pass, and fails as
# soon as one fails. The release workflow runs it on the pull requests it
# opens: merged with GITHUB_TOKEN, they start no other workflow, so the run
# carries on by itself.
set -euo pipefail
pr=$1

# Every 20 s for 15 min. A required check that has not reported yet is not
# listed: the merge is then refused, and tried again at the next turn.
for _ in $(seq 45); do
  status=0
  out=$(gh pr checks "$pr" --required 2>&1) || status=$?
  case $status in
    0) gh pr merge "$pr" --squash && exit 0 ;;
    # Some checks are still running.
    8) ;;
    *)
      if ! grep -q "no .*checks reported" <<<"$out"; then
        echo "$out" >&2
        echo "a required check of $pr failed" >&2
        exit 1
      fi
      ;;
  esac
  sleep 20
done
echo "$pr was not merged within 15 min" >&2
exit 1
