#!/usr/bin/env bash
#
# Give a Windows job the newest released LLVM, and refuse the job before it
# builds anything when this image's MSVC standard library will not accept that
# LLVM's clang-cl.
#
# skia-bindings compiles Skia with the clang-cl under `LLVM_HOME`, and only
# when that is unset under `C:\Program Files\LLVM`; bindgen loads libclang
# through clang-sys, which reads `LIBCLANG_PATH` first. All three, and `PATH`,
# are pointed at one unpacked release, so Skia, its bindings and anything that
# asks for `clang` see the same compiler. The image's own LLVM stays where it
# is and goes unused: the image does not keep it in step with its MSVC, whose
# STL names the oldest Clang it accepts and refuses every older one with
# STL1000 in `yvals_core.h` -- MSVC 14.52 wants Clang 22, beside the image's
# LLVM 20.
#
# The release is the newest stable one of llvm/llvm-project, by version, that
# has this architecture's archive and the attestation for it, so a release
# whose Windows archives are still being built is passed over rather than
# failing the job. The archive is held to that attestation -- its digest,
# signed by llvm-project's release-binaries workflow at the release's tag --
# before anything in it runs.
#
# The check that follows compiles one standard header with the chosen
# clang-cl, so the floor is the one this image's STL states rather than a
# number written here that the next MSVC moves. The image's own clang-cl, where
# there is one, takes the same check and only reports: on an image whose STL
# refuses it, that line is the check going red on a compiler known to be too
# old.
#
# Usage: .github/scripts/use-llvm.sh   (bash on a Windows runner, with GH_TOKEN)

set -euo pipefail

case "${RUNNER_ARCH:-}" in
    X64) triple=x86_64-pc-windows-msvc ;;
    ARM64) triple=aarch64-pc-windows-msvc ;;
    *)
        echo "::error::use-llvm: llvm-project publishes no Windows archive for runner architecture '${RUNNER_ARCH:-unset}'"
        exit 1
        ;;
esac

if ! command -v 7z > /dev/null; then
    echo "::error::use-llvm: 7z is not on PATH, and the LLVM archive is a .tar.xz that it unpacks"
    exit 1
fi

# Every stable release in the last page, oldest to newest by version: the API
# lists by creation date, and a point release of an older major can be created
# after a newer major's.
version=$(gh api 'repos/llvm/llvm-project/releases?per_page=30' --jq "
    .[] | select(.draft == false and .prerelease == false)
        | select(.tag_name | test(\"^llvmorg-[0-9]+\\\\.[0-9]+\\\\.[0-9]+\$\"))
        | (.tag_name | ltrimstr(\"llvmorg-\")) as \$v
        | (\"clang+llvm-\" + \$v + \"-$triple.tar.xz\") as \$a
        | select(any(.assets[]; .name == \$a) and any(.assets[]; .name == \$a + \".jsonl\"))
        | \$v" | sort -V | tail -n 1)
if [ -z "$version" ]; then
    echo "::error::use-llvm: no stable llvm-project release among the latest 30 has clang+llvm-<version>-$triple.tar.xz and its attestation"
    exit 1
fi
tag="llvmorg-$version"
archive="clang+llvm-$version-$triple.tar.xz"
echo "LLVM $version, $archive"

# RUNNER_TEMP is a Windows path; bash's tools take it in POSIX form, and the
# Windows programs given a path inside an option -- 7z's `-o` -- are handed it
# back in Windows form, which MSYS does not convert there.
temp=$(cygpath -u "$RUNNER_TEMP")
download="$temp/llvm-download"
unpacked="$temp/llvm"
rm -rf "$download" "$unpacked"
mkdir -p "$download" "$unpacked"
started=$SECONDS
gh release download "$tag" --repo llvm/llvm-project --dir "$download" \
    --pattern "$archive" --pattern "$archive.jsonl"
gh attestation verify "$download/$archive" --repo llvm/llvm-project \
    --bundle "$download/$archive.jsonl" \
    --signer-workflow llvm/llvm-project/.github/workflows/release-binaries.yml \
    --source-ref "refs/tags/$tag"
echo "downloaded and verified in $((SECONDS - started)) s"
started=$SECONDS

7z x -so "$(cygpath -w "$download/$archive")" | 7z x -si -ttar -o"$(cygpath -w "$unpacked")" -bso0 -bsp0
rm -rf "$download"
echo "unpacked in $((SECONDS - started)) s"

# The archive's top directory is named for the release; found rather than
# assumed, so a renamed one fails here and not inside skia-bindings.
clang_cl=$(find "$unpacked" -maxdepth 3 -path '*/bin/clang-cl.exe' | head -n 1)
if [ -z "$clang_cl" ]; then
    echo "::error::use-llvm: $archive holds no bin/clang-cl.exe"
    exit 1
fi
home=$(dirname "$(dirname "$clang_cl")")
"$clang_cl" --version | head -n 1

probe="$temp/stl-probe.cpp"
printf '#include <cstddef>\n' > "$probe"

# Compiles the probe with one clang-cl, syntax only, printing what it said.
# Its options in their dash spelling, which clang-cl takes as it takes the
# slash one: MSYS rewrites an argument starting with a slash into a path
# under Git's install, so `/Zs` reached clang-cl as `C:/Program Files/Git/Zs`
# and the link step it never meant to run failed on that file.
accepted() {
    "$1" -nologo -Zs -TP "$(cygpath -w "$probe")" 2>&1
}

image_clang_cl='/c/Program Files/LLVM/bin/clang-cl.exe'
if [ -x "$image_clang_cl" ]; then
    image_version=$("$image_clang_cl" --version | head -n 1)
    if accepted "$image_clang_cl" > /dev/null; then
        echo "control: this image's own $image_version is accepted by its STL too"
    else
        echo "control: this image's own $image_version is refused by its STL:"
        accepted "$image_clang_cl" | grep -m 1 'error' || true
    fi
fi

if ! said=$(accepted "$clang_cl"); then
    echo "$said"
    echo "::error::use-llvm: LLVM $version's clang-cl cannot compile <cstddef> against this image's MSVC, so Skia will not build with it. If the line above is STL1000, this MSVC needs a newer Clang than llvm-project has released for $triple."
    exit 1
fi
echo "LLVM $version's clang-cl compiles <cstddef> against this image's MSVC"

{
    echo "LLVM_HOME=$(cygpath -w "$home")"
    echo "LIBCLANG_PATH=$(cygpath -w "$home/bin")"
} >> "$GITHUB_ENV"
cygpath -w "$home/bin" >> "$GITHUB_PATH"
