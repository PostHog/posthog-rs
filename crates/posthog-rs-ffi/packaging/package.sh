#!/usr/bin/env bash
# Builds the release archive for one target from an existing release build.
#
#   packaging/package.sh <target> <version> <build-dir> <dist-dir>
#
# <build-dir> holds the built libraries and native-static-libs.txt, for example
# ../../target/x86_64-unknown-linux-gnu/release. Prints the archive's file name.
set -euo pipefail

target="$1"
version="$2"
out="$3"
dist="$4"

crate_dir="$(cd "$(dirname "$0")/.." && pwd)"
name="posthog-rs-ffi-$version-$target"
root="$dist/$name"
native_libs="$(tr -d '\r' < "$out/native-static-libs.txt")"

rm -rf "$root"
mkdir -p "$root/include" "$root/lib/cmake/posthog-rs-ffi" "$root/examples"
cp "$crate_dir/include/posthog.h" "$root/include/"
cp "$crate_dir/examples/capture.c" "$crate_dir/examples/capture.cpp" "$root/examples/"
cp "$crate_dir/README.md" "$root/"
cp "$crate_dir/../../LICENSE" "$root/"
cp "$out/native-static-libs.txt" "$root/lib/"

case "$target" in
    *windows*)
        cp "$out/posthog_ffi.dll" "$out/posthog_ffi.dll.lib" "$out/posthog_ffi.lib" "$root/lib/"
        shared_file=posthog_ffi.dll
        shared_soname=''
        static_file=posthog_ffi.lib
        ;;
    *apple*)
        cp "$out/libposthog_ffi.dylib" "$out/libposthog_ffi.a" "$root/lib/"
        shared_file=libposthog_ffi.dylib
        shared_soname='@rpath/libposthog_ffi.dylib'
        static_file=libposthog_ffi.a
        ;;
    *)
        cp "$out/libposthog_ffi.so" "$out/libposthog_ffi.a" "$root/lib/"
        shared_file=libposthog_ffi.so
        shared_soname='libposthog_ffi.so'
        static_file=libposthog_ffi.a
        ;;
esac

# CMake link items: one per library, keeping `-framework X` together.
cmake_libs=()
read -ra words <<< "$native_libs"
i=0
while [ "$i" -lt "${#words[@]}" ]; do
    word="${words[$i]}"
    if [ "$word" = "-framework" ]; then
        item="-framework ${words[$((i + 1))]}"
        i=$((i + 2))
    else
        item="$word"
        i=$((i + 1))
    fi
    seen=false
    for existing in "${cmake_libs[@]+"${cmake_libs[@]}"}"; do
        [ "$existing" = "$item" ] && seen=true
    done
    [ "$seen" = false ] && cmake_libs+=("$item")
done
# A CMake list is separated by semicolons, which keeps `-framework X` as one item.
cmake_libs_text=$(IFS=';'; echo "${cmake_libs[*]+"${cmake_libs[*]}"}")

sed -e "s|@SHARED_FILE@|$shared_file|" \
    -e "s|@SHARED_SONAME@|$shared_soname|" \
    -e "s|@STATIC_FILE@|$static_file|" \
    -e "s|@NATIVE_LIBS@|$cmake_libs_text|" \
    "$crate_dir/packaging/posthog-rs-ffi-config.cmake.in" \
    > "$root/lib/cmake/posthog-rs-ffi/posthog-rs-ffi-config.cmake"
sed -e "s|@VERSION@|$version|" \
    "$crate_dir/packaging/posthog-rs-ffi-config-version.cmake.in" \
    > "$root/lib/cmake/posthog-rs-ffi/posthog-rs-ffi-config-version.cmake"

# pkg-config is rarely used with MSVC, so Windows archives ship only CMake files.
if [[ "$target" != *windows* ]]; then
    mkdir -p "$root/lib/pkgconfig"
    for variant in posthog-rs-ffi posthog-rs-ffi-static; do
        sed -e "s|@VERSION@|$version|" -e "s|@NATIVE_LIBS@|$native_libs|" \
            "$crate_dir/packaging/$variant.pc.in" > "$root/lib/pkgconfig/$variant.pc"
    done
fi

cd "$dist"
if [[ "$target" == *windows* ]]; then
    archive="$name.zip"
    rm -f "$archive"
    7z a -tzip "$archive" "$name" >/dev/null
else
    archive="$name.tar.gz"
    tar -czf "$archive" "$name"
fi
if command -v sha256sum >/dev/null; then
    sha256sum "$archive" > "$archive.sha256"
else
    shasum -a 256 "$archive" > "$archive.sha256"
fi
echo "$archive"
