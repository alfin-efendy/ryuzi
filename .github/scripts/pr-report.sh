#!/usr/bin/env bash
# Composes the sticky PR comment body (stdout) from:
#   NEEDS_JSON — the `needs` context of the report job (job results),
#   osv-results.json — osv-scanner findings artifact, if present in $PWD,
#   the Actions API — last log lines of each failed job.
# Required env: NEEDS_JSON, GH_TOKEN, RUN_ID, REPO, RUN_URL.
set -euo pipefail

emoji() {
  case "$1" in
    success) echo "✅" ;;
    failure) echo "❌" ;;
    cancelled) echo "🚫" ;;
    skipped) echo "⏭️" ;;
    *) echo "❔" ;;
  esac
}

echo "## CI report"
echo
echo "| Job | Result |"
echo "| --- | --- |"
for job in lint ts rust e2e-cockpit osv; do
  result=$(jq -r --arg j "$job" '.[$j].result // "skipped"' <<<"$NEEDS_JSON")
  echo "| \`$job\` | $(emoji "$result") $result |"
done
echo
echo "[Full run]($RUN_URL)"

if [ -s osv-results.json ]; then
  count=$(jq '[.results[]?.packages[]?.vulnerabilities[]?] | length' osv-results.json)
  if [ "${count:-0}" -gt 0 ]; then
    echo
    echo "### 🔒 osv-scanner: ${count} finding(s)"
    echo
    echo "| Package | Version | Ecosystem | Advisory | Fixed in |"
    echo "| --- | --- | --- | --- | --- |"
    jq -r '
      .results[]?.packages[]? as $p
      | $p.vulnerabilities[]?
      | [
          $p.package.name,
          $p.package.version,
          $p.package.ecosystem,
          "[\(.id)](https://osv.dev/vulnerability/\(.id))",
          ([.affected[]?.ranges[]?.events[]?.fixed // empty] | first // "n/a")
        ]
      | "| " + join(" | ") + " |"
    ' osv-results.json
  fi
fi

failed=$(jq -r 'to_entries[] | select(.value.result == "failure") | .key' <<<"$NEEDS_JSON")
if [ -n "$failed" ]; then
  # Guarded: an unreachable/unauthorized Actions API must not abort the script
  # via `set -e` — the table and osv sections above must still ship in the
  # comment even if log-tail lookup can't run. `gh api` writes its error body
  # to stdout on failure, so on error the captured output is discarded
  # entirely rather than appended to, to avoid producing malformed JSON.
  if ! jobs_json=$(gh api "repos/$REPO/actions/runs/$RUN_ID/jobs?per_page=100" 2>/dev/null); then
    jobs_json='{"jobs":[]}'
  fi
  for name in $failed; do
    # Matrix jobs render as "rust (ubuntu-latest)" — match by prefix.
    for id in $(jq -r --arg n "$name" \
        '.jobs[] | select(.name == $n or (.name | startswith($n + " "))) | select(.conclusion == "failure") | .id' \
        <<<"$jobs_json"); do
      jobname=$(jq -r --argjson i "$id" '.jobs[] | select(.id == $i) | .name' <<<"$jobs_json")
      echo
      echo "<details><summary>❌ <code>${jobname}</code> — last 40 log lines</summary>"
      echo
      echo '```'
      gh api "repos/$REPO/actions/jobs/$id/logs" | tail -n 40 | sed -E 's/^[0-9TZ:.-]+ //' || echo "(logs unavailable)"
      echo '```'
      echo
      echo "</details>"
    done
  done
fi
