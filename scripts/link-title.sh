#!/usr/bin/env bash
# Links the Rust title (a static library) with RADV into eboot.bin and lays out the title
# folder: dist/<TITLE_ID>/{eboot.bin, sce_sys/param.json, sce_sys/icon0.png, sce_module/libc.prx}.
#
#   scripts/link-title.sh <libpf_ps5_title.a> <TITLE_ID> [name]
#
# The link is PS5_Vulkan's radv_link_recipe (tools/radv-link.sh at the pinned revision): its CRT,
# RADV linked whole, libc++, the SDK fork's platform layer, and the wraps and defsyms that bind
# libc names the console gets wrong. This adds the client's own bindings (pf-ps5-sys `shim`) and
# the stub for Videodec2, which the SDK has none of.
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/pins.env
source "$root/scripts/pins.env"
lib=${1:?usage: link-title.sh <staticlib> <TITLE_ID> [name]}
title_id=${2:?usage: link-title.sh <staticlib> <TITLE_ID> [name]}
name=${3:-Punktfunk}
[[ $title_id =~ ^PPSA[0-9]{5}$ ]] || { echo "TITLE_ID must look like PPSA12345" >&2; exit 2; }

tc="$root/.toolchains"
vk="$tc/PS5_Vulkan"
sdk="$vk/.deps/native/ps5-payload-sdk"
archive="$vk/.deps/native/radv-release/lib/libvulkan_radeon.ps5.a"
tool="$tc/bin/ps5-native-tool"
work="$root/target/ps5-link/$title_id"
for f in "$lib" "$archive" "$tool" "$sdk/bin/prospero-lld" "$vk/runtime/libc.prx"; do
    [[ -e $f ]] || { echo "missing $f (run task toolchain)" >&2; exit 2; }
done
mkdir -p "$work/obj" "$work/stubs"
cc() { PS5_PAYLOAD_SDK="$sdk" sh "$vk/tooling/prospero-clang18" "$@"; }

cc -std=c++20 -O2 -fno-exceptions -fno-rtti -c "$vk/tooling/native/app_crt.cpp" -o "$work/obj/app_crt.o"
cc -std=c++20 -O2 -fno-exceptions -fno-rtti -c "$vk/tooling/native/app_cpp_runtime.cpp" -o "$work/obj/app_cpp_runtime.o"

# Link-only stubs: symbol names, no code. The console's modules provide the bodies.
stub() {
    local library=$1 source=$2
    cc -std=c11 -O2 -fPIC -c "$source" -o "$work/obj/${library}_stub.o"
    "$sdk/bin/prospero-lld" --shared -soname "${library}.prx" -o "$work/stubs/${library}.so" "$work/obj/${library}_stub.o"
}
stub libSceAgc "$vk/vendor/ps5/sdk/stubs/agc_canary_link_stub.c"
stub libSceAgcDriver "$vk/vendor/ps5/sdk/stubs/agc_driver_canary_link_stub.c"
stub libSceVideodec2 "$root/scripts/stubs/videodec2.c"

# shellcheck source=/dev/null
source "$vk/tools/radv-link.sh"
radv_link_recipe "$vk" "$sdk" "$archive"

# The client's own bindings (pf-ps5-sys `shim`), kept local like the recipe's.
shims=(pthread_set_name_np pthread_setname_np getentropy getrandom recvmmsg if_nametoindex minherit uselocale dl_iterate_phdr
    gai_strerror)
# The recipe binds these itself once the SDK pin carries them.
for s in mkstemp readlink; do
    [[ " ${radv_link_flags[*]} " == *" --defsym=$s="* ]] || shims+=("$s")
done
own=()
for s in "${shims[@]}"; do own+=("--defsym=$s=pf_ps5_$s"); done
own+=(--defsym=_Z11SkLogVAList13SkLogPriorityPKcP13__va_list_tag=pf_ps5_sk_log_va_list)
# Socket flags through the console's SO_NBIO where fcntl and FIONBIO are refused, shared
# descriptors where it refuses a duplicate, socket buffers its cap refuses (shim.rs).
own+=(--wrap=fcntl --wrap=ioctl --wrap=close --wrap=setsockopt)
{ echo '{'; echo '    local:'; printf '        %s;\n' "${shims[@]}"; echo '};'; } > "$work/own-local.map"

# Rust's FreeBSD 11 bindings name these by symbol version; the console's stubs carry none.
title="$work/title.a"
renames=()
for s in stat fstat lstat readdir kevent; do renames+=("--redefine-sym=$s@FBSD_1.0=$s"); done
"$sdk/bin/llvm-objcopy" "${renames[@]}" "$lib" "$title"

# A weak reference nothing defines (Mesa's entry points, aws-lc's allocator hooks) is null, not an
# import the converter needs a stub for. LLD 18 needs --no-dynamic-linker for that, LLD 22 the -z.
"$sdk/bin/prospero-lld" "${radv_linker_script[@]}" --eh-frame-hdr "${radv_link_flags[@]}" "${own[@]}" \
    --no-dynamic-linker -z nodynamic-undefined-weak \
    --version-script "$work/own-local.map" \
    --version-script "$vk/tooling/native/app-symbols.map" --exclude-libs=ALL \
    -e _start -o "$work/llvm-pie.elf" \
    "$work/obj/app_crt.o" "$work/obj/app_cpp_runtime.o" "$title" \
    "$work/stubs/libSceAgc.so" "$work/stubs/libSceAgcDriver.so" "$work/stubs/libSceVideodec2.so" \
    "${radv_link_inputs[@]}" \
    --as-needed "$sdk"/target/lib/*.so

# An import whose first definer (link order) is one of these stubs links and is null at run time.
dead=$(
    for s in "$sdk"/target/lib/*.so; do
        "$sdk/bin/llvm-nm" -D --defined-only "$s" | awk -v lib="${s##*/}" '{ print $NF, lib }'
    done | awk 'NR == FNR { if (!($1 in owner)) owner[$1] = $2; next }
        owner[$NF] ~ /^lib(ScePosixForWebKit|kernel_sys)\.so$/ { print "  " $NF " (" owner[$NF] ")" }' \
        - <("$sdk/bin/llvm-nm" -D --undefined-only "$work/llvm-pie.elf")
)
[[ -z $dead ]] || { printf 'imports a title gets nothing from; shim them in pf-ps5-sys:\n%s\n' "$dead" >&2; exit 1; }

"$tool" link --in "$work/llvm-pie.elf" --out "$work/eboot.elf" \
    --stub-dir "$sdk/target/lib" \
    --stub "$work/stubs/libSceAgc.so" --stub "$work/stubs/libSceAgcDriver.so" --stub "$work/stubs/libSceVideodec2.so" \
    --module-sdk 0x02000009 --companion-sdk 0x08050001 --file-name eboot.elf

app="$root/dist/$title_id"
rm -rf -- "$app"
mkdir -p "$app/sce_sys" "$app/sce_module"
"$tool" self --sign --in "$work/eboot.elf" --out "$app/eboot.bin" --magic 0x1D3D154F
python3 "$root/scripts/render-param.py" "$root/packaging/sce_sys/param.json" "$title_id" "$name" > "$app/sce_sys/param.json"
cp "$root/packaging/sce_sys/icon0.png" "$app/sce_sys/icon0.png"
cp "$vk/runtime/libc.prx" "$app/sce_module/libc.prx"
cp "$work/llvm-pie.elf" "$root/dist/$title_id.llvm-pie.elf"
"$tool" self --inspect --file "$app/eboot.bin" > /dev/null
printf 'title %s: %s (%s bytes); symbols in dist/%s.llvm-pie.elf\n' "$title_id" "$app" "$(stat -c %s "$app/eboot.bin")" "$title_id"
