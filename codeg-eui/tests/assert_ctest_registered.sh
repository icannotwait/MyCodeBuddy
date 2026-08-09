#!/bin/sh
set -eu

build_dir=$1
shift
for exact_name do
  count=$(ctest --test-dir "$build_dir" -N -R "^${exact_name}$" |
    awk '/Total Tests:/ { print $3 }')
  test "$count" = 1 || {
    printf 'expected one CTest named %s, found %s\n' \
      "$exact_name" "${count:-0}" >&2
    exit 1
  }
done
