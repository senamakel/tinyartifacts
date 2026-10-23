#!/usr/bin/env bash
set -euo pipefail

readonly forbidden='^(rusqlite|libsqlite3-sys|mongodb|bson|aws-sdk-s3|lapin|amq-protocol|sqlx|redb|sled|rocksdb|tinystoragedrivers-(document|kv|blob|queue|secrets|connect)|tinystoragedrivers)$'

tree_names() {
  awk 'NF { print $1 }'
}

has_forbidden() {
  local names
  names="$(tree_names <<< "$1")"
  [[ -n "$names" ]] || return 2
  grep -Exq "$forbidden" <<< "$names"
}

self_test() {
  local allowed forbidden_engine forbidden_tsd
  allowed=$'tinystoragedrivers-types v2.0.1 (git+https://example.invalid/repo)\ntinystoragedrivers-api v2.0.1'
  forbidden_engine=$'tiny-artifacts v0.1.0 (/workspace/crates/tiny-artifacts)\nrusqlite v0.40.2'
  forbidden_tsd=$'tiny-artifacts v0.1.0 (/workspace/crates/tiny-artifacts)\ntinystoragedrivers-document v2.0.1 (git+https://example.invalid/repo)'

  if has_forbidden "$allowed"; then
    echo "engine guard rejected allowed API/types dependencies" >&2
    return 1
  fi
  if ! has_forbidden "$forbidden_engine"; then
    echo "engine guard missed a versioned engine tree row" >&2
    return 1
  fi
  if ! has_forbidden "$forbidden_tsd"; then
    echo "engine guard missed a path-qualified TSD engine tree row" >&2
    return 1
  fi
  if has_forbidden ""; then
    echo "engine guard accepted empty tree output" >&2
    return 1
  else
    local status=$?
    if [[ $status -ne 2 ]]; then
      echo "engine guard failed to reject an empty tree predictably" >&2
      return 1
    fi
  fi
  echo "storage engine guard self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit
fi

self_test
packages="$(cargo metadata --locked --no-deps --format-version 1 \
  | jq -er '[.packages[] | select(any(.targets[]; any(.kind[]; . == "lib" or . == "cdylib" or . == "staticlib"))) | .name] | unique | .[]')"
[[ -n "$packages" ]] || { echo "workspace metadata contains no library targets" >&2; exit 1; }

while IFS= read -r package; do
  [[ -n "$package" ]] || continue
  names="$(cargo tree --locked --package "$package" --all-features --edges normal,build --prefix none | tree_names)"
  [[ -n "$names" ]] || { echo "cargo tree returned no packages for $package" >&2; exit 1; }
  if grep -Exq "$forbidden" <<< "$names"; then
    printf 'storage engine dependency found in %s:\n' "$package" >&2
    grep -Ex "$forbidden" <<< "$names" >&2
    exit 1
  fi
  printf 'storage dependency boundary passed: %s\n' "$package"
done <<< "$packages"
