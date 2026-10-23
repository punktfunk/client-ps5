#!/usr/bin/env bash
# Installs the PS5 toolchain under .toolchains/: the pinned PS5_Vulkan tree with its Mesa and
# payload-SDK forks beside it, the SDK and RADV built by that tree's own scripts, the title
# converter (ps5-native-tool), the libc.prx companion, and the pinned nightly with rust-src.
# Idempotent: a finished step is skipped. Runs in the Docker image (task docker:toolchain).
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/pins.env
source "$root/scripts/pins.env"
tc="$root/.toolchains"
mkdir -p "$tc/bin"

fetch() {
    local repo=$1 rev=$2 dir=$3
    if [[ ! -d $dir/.git ]]; then
        git clone --quiet --filter=blob:none "$repo" "$dir"
    fi
    if [[ $(git -C "$dir" rev-parse HEAD) != "$rev" ]]; then
        git -C "$dir" remote set-url origin "$repo"
        git -C "$dir" fetch --quiet origin "$rev" || git -C "$dir" fetch --quiet origin
        git -C "$dir" checkout --quiet --detach "$rev"
    fi
    echo "==> $(basename "$dir") at ${rev:0:12}"
}

fetch "$PS5_VULKAN_REPO" "$PS5_VULKAN_REV" "$tc/PS5_Vulkan"
fetch "$PS5_MESA_REPO" "$PS5_MESA_REV" "$tc/PS5_Mesa"
fetch "$PS5_SDK_REPO" "$PS5_SDK_REV" "$tc/PS5_PayloadSDK"

vk="$tc/PS5_Vulkan"
PS5_PAYLOAD_SDK_FORK="$tc/PS5_PayloadSDK" bash "$vk/tools/setup-native-dependencies.sh"
PS5_MESA_FORK="$tc/PS5_Mesa" PS5_MESA_REVISION="$PS5_MESA_REV" bash "$vk/tools/build-radv.sh" release

# The title converter, built the way PS5_Vulkan's tools/build.sh builds it.
tool="$tc/bin/ps5-native-tool"
if [[ ! -x $tool ]]; then
    zlib=$(find "$vk/.deps/native/zlib/root" -type f -name libz.a -print -quit)
    native="$vk/tooling/native"
    ${CXX:-clang++} -std=c++20 -O2 -I "$vk/.deps/native/zlib/root/usr/include" \
        "$native/native_app_builder.cpp" "$native/self_container.cpp" \
        "$native/elf_object.cpp" "$native/sce_module_writer.cpp" "$zlib" -o "$tool"
fi
echo "==> ps5-native-tool"

[[ -f $vk/runtime/libc.prx ]] || bash "$vk/tools/rebuild-libc.sh"
echo "==> libc.prx $(sha256sum "$vk/runtime/libc.prx" | cut -c1-12)"

rustup toolchain install "$RUST_NIGHTLY" --profile minimal --component rust-src,rustfmt,clippy
# std reads FreeBSD 12's 64-bit inode numbers; the console's FreeBSD 11 libc has 32-bit ones.
std_fs="$(rustc "+$RUST_NIGHTLY" --print sysroot)/lib/rustlib/src/rust/library/std/src/sys/fs/unix.rs"
sed -i 's/d_ino: (\*entry_ptr).d_fileno,/d_ino: (*entry_ptr).d_fileno as u64,/' "$std_fs"
grep -q 'd_ino: (\*entry_ptr).d_fileno as u64,' "$std_fs" || { echo "std's dirent patch did not apply" >&2; exit 1; }
echo "==> rust $RUST_NIGHTLY"
