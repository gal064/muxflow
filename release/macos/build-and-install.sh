#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "$0")" && pwd -P)

"$script_dir/build-package.sh"
"$script_dir/install.sh"
