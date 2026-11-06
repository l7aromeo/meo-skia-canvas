#!/usr/bin/env bash
#
# Install what it takes to open a window and create a Vulkan instance on a
# GitHub runner with no screen and no GPU.
#
# `mesa-vulkan-drivers` supplies lavapipe, a software Vulkan device, so a
# Vulkan instance can be created and the check that reports "No windowing
# support: Vulkan initialization failed" passes. `xvfb` supplies the display.
# The X client libraries are the part that is easy to miss: with only
# `libx11-xcb1` a window still *registers* -- `openWindow` records the spec
# and returns -- and then `App.launch` fails to build the winit loop and
# reports no display, on a runner whose display is fine. Verified in a
# container: with this list the loop runs and delivers frames, without it the
# launch fails.
#
# The list was verified as a set. No package here was tested for individual
# necessity, so trimming one because it looks unrelated is untested either
# way -- and it will still pass on a developer machine, which has a display
# of its own.
#
# One list for every workflow that runs the crate's suite with `vulkan`:
# rust-ci.yml's binding job and crates-io-publish.yml. The publish ran the
# suite without it once, and a test that creates a Vulkan instance failed
# there although it passed in rust-ci (#613).
#
# Usage: .github/scripts/install-software-gpu.sh

set -euo pipefail

"$(dirname "$0")/apt-install.sh" \
    xvfb mesa-vulkan-drivers libvulkan1 \
    libxkbcommon-x11-0 libx11-xcb1 libxcb-cursor0 libxcb-icccm4 \
    libxcb-randr0 libxcb-xkb1 libxcb-shape0 libxcb-xfixes0 \
    libxcursor1 libxrandr2 libxi6 libxinerama1
