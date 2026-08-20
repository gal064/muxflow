#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd -P)
cd "$repo_root"

# Keep the denied values split so this guard does not match its own source.
host_alias='omar''chy'
host_alias_short='mar''chy'
local_account='dev''vered'
linux_home='/home/''dev'
mac_home='/Users/''dev'
private_repo='muxflow-''monorepo'
private_fixture='sampleco-''e2e'
old_checkout='dev-''ade'
old_integration='.or''ca'
private_network='tail''scale'
personal_namespace='dev.''dev.'
personal_email='gal064''@gmail.com'
private_ip='100.81.''245.89'

setup_pattern="${host_alias}|${host_alias_short}|${local_account}|${linux_home}(/|$)|${mac_home}(/|$)|${private_repo}|${private_fixture}|${old_checkout}|${old_integration}(/|$)|${private_network}|${personal_namespace}|${personal_email}|${private_ip}"

private_key='BEGIN .*PRIVATE ''KEY'
aws_key='AKIA[A-Z0-9]{16}'
github_token='gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}'
slack_token='xox[baprs]-[A-Za-z0-9-]{10,}'
bearer_token='Bearer [A-Za-z0-9._~+/=-]{20,}'
secret_pattern="${private_key}|${aws_key}|${github_token}|${slack_token}|${bearer_token}"

failed=0
while IFS= read -r -d '' file; do
  [[ -f "$file" ]] || continue
  if LC_ALL=C grep -IHEni -- "$setup_pattern" "$file"; then
    failed=1
  fi
  if LC_ALL=C grep -IHEn -- "$secret_pattern" "$file"; then
    failed=1
  fi
done < <(git ls-files --cached --others --exclude-standard -z)

if ((failed)); then
  echo 'repository privacy check failed' >&2
  exit 1
fi

echo 'REPO_PRIVACY_OK'
