#!/usr/bin/env bash
#
# Install apt packages on a GitHub runner, without letting a repository this
# project does not use decide whether the build runs.
#
# `apt-get update` fetches every configured index and exits non-zero when any
# one of them fails. GitHub's Ubuntu image ships third-party sources nobody
# here asked for, and on 2026-09-10 Google's served a corrupt index:
#
#     Err:24 https://dl.google.com/linux/chrome-stable/deb stable/main amd64
#     E: Failed to fetch .../Packages.gz  Hash Sum mismatch
#
# Every Linux job in `rust-ci.yml` failed on that, at the apt step, in a build
# that wants Skia and has no opinion about Chrome. Both macOS jobs passed,
# which is what identified it as the runner image rather than this tree.
#
# Removing the list is narrower than the two obvious alternatives. `apt-get
# update || true` would also swallow a genuine failure -- a mirror outage for
# a package we really need -- and leave the install to fail later with a
# worse message. Retrying the update only helps if the next attempt reaches a
# different CDN node, which is a coin flip rather than a fix. Neither stops
# the class; this does, because an index that is not configured cannot fail.
#
# `Acquire::Retries` is kept as well, for the ordinary transient case: a reset
# connection to a mirror we do use, which retrying genuinely does fix.
#
# Nothing here hides a real problem. If a package this project needs is
# missing or a mirror we depend on is down, `apt-get install` still fails and
# still says which package.
#
# Usage: .github/scripts/apt-install.sh <package>...

set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "apt-install: no packages given" >&2
    exit 1
fi

# Only the sources this project does not use. Listed rather than globbed:
# clearing `sources.list.d` wholesale would also drop anything a future step
# deliberately adds, and the failure would be a missing package with no clue
# as to why.
sudo rm -f /etc/apt/sources.list.d/google-chrome.list \
    /etc/apt/sources.list.d/google-chrome-*.list

sudo apt-get update -o Acquire::Retries=3
sudo apt-get install -y --no-install-recommends -o Acquire::Retries=3 "$@"
