#!/usr/bin/env bash
# Shared reproducibility evidence for every Phase 7 acceptance gate.

phase7_source_manifest() {
  local repo_root="$1"
  local manifest="$2"
  (
    cd "$repo_root"
    if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
      git ls-files --cached --others --exclude-standard -z
    else
      find . \
        -type d \( -name .git -o -name tmp -o -name target -o -name dist \
          -o -name node_modules -o -name coverage -o -name generated \) -prune -o \
        -type f -print0
    fi | LC_ALL=C sort -z |
      while IFS= read -r -d '' path; do
        hash="$(sha256sum -- "$path" | cut -d ' ' -f1)"
        printf '%s\t%q\n' "$hash" "${path#./}"
      done
  ) >"$manifest"
}

# Stable reuse key for the expensive exact-size acceptance. Deliberately bind
# to production transfer/protocol code and the compiled acceptance driver, not
# Markdown, screenshots, or shell orchestration policy. Change the explicit
# contract version in phase7_embed_source_digest when the acceptance semantics
# change even if these paths do not.
phase7_transfer_component_manifest() {
  local repo_root="$1"
  local manifest="$2"
  (
    cd "$repo_root"
    find \
      Cargo.lock Cargo.toml rust-toolchain.toml \
      apps/host/Cargo.toml \
      apps/host/src/service/filesystem.rs apps/host/src/service/filesystem \
      apps/host/src/service/requests/filesystem_dispatch.rs \
      apps/desktop/src-tauri/Cargo.toml apps/desktop/src-tauri/build.rs \
      apps/desktop/src-tauri/src/connection/files.rs \
      apps/desktop/src-tauri/src/connection/files \
      crates/protocol tests/integration/transfers/protocol-driver/Cargo.toml \
      tests/integration/transfers/protocol-driver/src \
      -type f -not -path '*/target/*' -not -path '*/tmp/*' -print0 \
      | LC_ALL=C sort -z \
      | while IFS= read -r -d '' path; do
          hash="$(sha256sum -- "$path" | cut -d ' ' -f1)"
          printf '%s\t%q\n' "$hash" "${path#./}"
        done
  ) >"$manifest"
}

phase7_capture_source_tree() {
  local repo_root="$1"
  local evidence="$2"
  phase7_source_manifest "$repo_root" "$evidence/source-tree-manifest.tsv"
  sha256sum "$evidence/source-tree-manifest.tsv" | cut -d ' ' -f1 \
    >"$evidence/source-tree-digest.txt"
  phase7_transfer_component_manifest \
    "$repo_root" "$evidence/transfer-component-manifest.tsv"
  sha256sum "$evidence/transfer-component-manifest.tsv" | cut -d ' ' -f1 \
    >"$evidence/transfer-component-digest.txt"
}

phase7_assert_source_tree_unchanged() {
  local repo_root="$1"
  local evidence="$2"
  phase7_source_manifest "$repo_root" "$evidence/source-tree-manifest.after.tsv"
  cmp "$evidence/source-tree-manifest.tsv" "$evidence/source-tree-manifest.after.tsv"
  rm "$evidence/source-tree-manifest.after.tsv"
}

phase7_embed_source_digest() {
  local evidence="$1"
  local result="$2"
  local digest transfer_digest temporary
  digest="$(<"$evidence/source-tree-digest.txt")"
  transfer_digest="$(<"$evidence/transfer-component-digest.txt")"
  temporary="$evidence/result-with-source-digest.json"
  jq --arg digest "$digest" --arg transferDigest "$transfer_digest" \
    '. + {sourceTreeDigest: $digest, transferComponentDigest: $transferDigest,
      acceptanceContractVersion: "phase7-exact-transfer-v1"}' \
    "$result" >"$temporary"
  mv "$temporary" "$result"
  jq -e --arg digest "$digest" --arg transferDigest "$transfer_digest" \
    '.sourceTreeDigest == $digest and .transferComponentDigest == $transferDigest and
      .acceptanceContractVersion == "phase7-exact-transfer-v1"' "$result" >/dev/null
}

phase7_wait_all() {
  local status=0
  local pid
  for pid in "$@"; do
    wait "$pid" || status=1
  done
  return "$status"
}
