#!/usr/bin/env bash
#
# Asserts that the toolchain inside a build container still emits binaries under
# the ABI floors this project commits to, without building Skia.
#
# WHAT THIS COVERS, AND WHAT IT DOES NOT. build.yml asserts the floors on the
# real artifact after a full build, and loads it on AlmaLinux 8 to prove every
# C++ symbol resolves there. That is the authority and this does not replace it.
# But build.yml is `workflow_dispatch` only, so a change to
# containers/Dockerfile.glibc -- a base image bump, a different gcc-toolset, an
# edited RUSTFLAGS -- reaches a release without any pull request having checked
# it. This runs on those pull requests, in the image the change produces.
#
# It checks the TOOLCHAIN's floor, not the artifact's. Our own Rust code or a
# dependency introducing a newer C++ symbol is not covered and cannot be:
# that needs the real build. What is covered is every way the container itself
# can raise the floor, which is the axis a pull request actually moves.
#
# WHY A RUST CDYLIB AND NOT A C++ TRANSLATION UNIT. The documented failure is
# specific to how rustc links. `-static-libstdc++` is dropped when rustc links
# through `cc`, the C driver, and takes effect only once the linker is clang++
# -- which is what `ENV RUSTFLAGS` in the Dockerfile sets and what a careless
# edit would undo. A `clang++` probe would link correctly and pass while the
# real build regressed. So the probe links the way the build links.
#
# The C++ side uses std::string deliberately. `_M_replace_cold` arrived in
# GCC 12 and carries no GLIBCXX_ version tag, so a symbol-version ceiling
# cannot see it -- a build once reported GLIBCXX_3.4.21, under every ceiling,
# and still failed to load. std::string operations are what pull that class of
# symbol in, and the load test below is what catches them.
#
# Usage:  containers/abi-probe.sh          (inside a build container)
#
set -euo pipefail

MAX_GLIBC=2.34
MAX_GLIBCXX=3.4.25

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

cat > probe.cc <<'CC'
#include <string>
// std::string operations are what pull in the untagged GCC 12 symbols that a
// version ceiling cannot see; a bare return would link against almost nothing
// and pass regardless of the toolset.
extern "C" unsigned long meo_abi_probe(void) {
  std::string s = "meo-skia-canvas abi probe";
  s.replace(0, 3, "MEO");
  s += std::to_string(s.size());
  return s.size();
}
CC

mkdir -p probe/src
cat > probe/Cargo.toml <<'TOML'
[package]
name = "abi-probe"
version = "0.0.0"
edition = "2021"
[lib]
crate-type = ["cdylib"]
[workspace]
TOML

cat > probe/src/lib.rs <<'RS'
extern "C" {
    fn meo_abi_probe() -> usize;
}
/// Called through `dlopen` by the load test; the `unsafe` block is the point of
/// the probe, since it is what forces the C++ side to be linked in at all.
#[no_mangle]
pub extern "C" fn probe() -> usize {
    unsafe { meo_abi_probe() }
}
RS

echo "compiling the C++ side with ${CXX:-clang++}"
"${CXX:-clang++}" -std=c++20 -fPIC -O2 -c probe.cc -o probe.o

# Linked exactly as the real build links: whatever RUSTFLAGS the image sets,
# plus the object. If the image stopped setting `-C linker=clang++`, the
# `-static-libstdc++` in it stops taking effect and this binary's floor moves.
echo "RUSTFLAGS in this image: ${RUSTFLAGS:-<unset>}"
cd probe
# `-lstdc++` is required and is not a detail. `-static-libstdc++` in the image's
# RUSTFLAGS governs only HOW libstdc++ is linked if something asks for it, and
# rustc never asks on its own -- the real build gets it because skia-bindings
# emits the link directive. Without it this probe links no C++ runtime at all
# and every C++ symbol comes back undefined, INCLUDING `_M_replace_cold`, which
# looks exactly like the historical regression this check exists to catch. That
# is a false positive convincing enough to act on; it was hit while writing this.
RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=$WORK/probe.o -C link-arg=-lstdc++" cargo build --release --quiet
SO=$(ls target/release/libabi_probe.so)

refs=$(objdump -p "$SO" | sed -n '/Version References/,/^$/p')
# `|| true` on the grep is load-bearing under `set -euo pipefail`: grep exits 1
# when it matches nothing, which makes the whole pipeline -- and therefore the
# assignment -- fail, killing the script one line before it would have reported.
# An absent GLIBCXX is the EXPECTED result here, not an error: a fully absorbed
# libstdc++ leaves no version references at all.
highest() { printf '%s' "$refs" | { grep -oE "$1"'_[0-9.]+' || true; } | sed "s/$1"'_//' | sort -V | tail -1; }
exceeds() { [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -1)" != "$1" ]; }

GLIBC=$(highest GLIBC)
GLIBCXX=$(highest GLIBCXX)
echo "toolchain emits GLIBC_${GLIBC:-none} (ceiling $MAX_GLIBC), GLIBCXX_${GLIBCXX:-none} (ceiling $MAX_GLIBCXX)"

fail=0
# An empty read is not portability; it means this stopped reading the binary.
if [ -z "$GLIBC" ]; then
  echo "::error::No GLIBC version references found at all -- this probe is no longer reading its own output. Investigate before trusting a pass."
  fail=1
elif exceeds "$MAX_GLIBC" "$GLIBC"; then
  echo "::error::The toolchain in this image emits GLIBC_$GLIBC, above the $MAX_GLIBC commitment. AWS Lambda and RHEL 9 are both 2.34 and would fail to load rather than degrade. Lower the base image in containers/Dockerfile.glibc, or change the commitment deliberately and say so in docs/getting-started.md. See #7."
  fail=1
fi
if [ -n "$GLIBCXX" ] && exceeds "$MAX_GLIBCXX" "$GLIBCXX"; then
  echo "::error::The toolchain emits GLIBCXX_$GLIBCXX, above the $MAX_GLIBCXX commitment. RHEL 8 ships 3.4.25 and would fail to load. This usually means gcc-toolset stopped linking its newer libstdc++ statically; check the toolset and RUSTFLAGS in containers/Dockerfile.glibc. See #7."
  fail=1
fi

# The check a version ceiling structurally cannot make. This container is
# AlmaLinux 8, so its libstdc++ is the oldest supported platform's: every
# undefined C++ symbol resolving here IS the RHEL 8 guarantee, including the
# untagged ones no ceiling above can see.
#
# A C loader rather than node, because `process.dlopen` expects a Node addon and
# this probe deliberately is not one -- it would fail on the missing module
# registration and say nothing about the ABI.
cat > "$WORK/loader.c" <<'LOADER'
#include <dlfcn.h>
#include <stdio.h>
int main(int argc, char **argv) {
  void *h = dlopen(argv[1], RTLD_NOW);
  if (!h) { printf("dlopen failed: %s\n", dlerror()); return 1; }
  unsigned long (*f)(void) = dlsym(h, "probe");
  if (!f) { printf("dlsym failed: %s\n", dlerror()); return 1; }
  printf("the toolchain's output loads and runs on the oldest supported platform (probe() = %lu)\n", f());
  return 0;
}
LOADER
"${CC:-clang}" "$WORK/loader.c" -o "$WORK/loader" -ldl
if ! "$WORK/loader" "$PWD/$SO"; then
  echo "::error::A binary from this toolchain does not load on AlmaLinux 8, the oldest platform this project claims. The version ceilings above passed, so this is an unversioned symbol they cannot see -- read the dlopen error just above. Usually libstdc++: rustc links through cc by default, which ignores -static-libstdc++, so RUSTFLAGS in containers/Dockerfile.glibc must also set the linker to clang++. See #7."
  fail=1
fi

exit $fail
