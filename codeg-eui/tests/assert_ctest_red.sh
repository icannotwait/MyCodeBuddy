#!/bin/sh
set -eu

build_dir=$1
exact_name=$2
failed_case=$3
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
"$script_dir/assert_ctest_registered.sh" "$build_dir" "$exact_name"
set +e
output=$(ctest --test-dir "$build_dir" -R "^${exact_name}$" \
  --output-on-failure 2>&1)
status=$?
set -e
test "$status" -ne 0
printf '%s\n' "$output" | grep -F "[FAIL] $failed_case" >/dev/null
printf '%s\n' "$output" |
  grep -F '0% tests passed, 1 tests failed out of 1' >/dev/null
