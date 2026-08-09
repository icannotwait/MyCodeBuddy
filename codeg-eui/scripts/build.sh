#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd -P)

if [ "$(uname -s)" != Linux ]; then
  printf '%s\n' 'codeg-eui is supported only on Linux' >&2
  exit 1
fi

eui_dir="$repo_root/codeg-eui/third_party/EUI-NEO"
expected_eui_commit=cb70ea8bea263efa7805a40c07135df028ad44b1
actual_eui_commit=$(git -C "$eui_dir" rev-parse HEAD 2>/dev/null || true)
if [ "$actual_eui_commit" != "$expected_eui_commit" ]; then
  printf 'EUI-NEO must be initialized at %s; found %s\n' \
    "$expected_eui_commit" "${actual_eui_commit:-uninitialized}" >&2
  exit 1
fi

cargo build \
  --manifest-path "$repo_root/src-tauri/codeg-eui-core/Cargo.toml" \
  --release

rust_lib="$repo_root/src-tauri/codeg-eui-core/target/release/libcodeg_eui_core.a"
build_dir="$repo_root/codeg-eui/build"
cmake -S "$repo_root/codeg-eui" -B "$build_dir" \
  -DCMAKE_BUILD_TYPE=Release \
  -DEUI_WINDOW_BACKEND=glfw \
  -DEUI_RENDER_BACKEND=opengl \
  -DCODEG_EUI_RUST_LIB="$rust_lib"
cmake --build "$build_dir" --parallel

printf '%s\n' "$build_dir/codeg-eui"
