#!/usr/bin/env bash
set -euxo pipefail

export CC=clang
export CXX=clang++

# Fetch a tarball and unpack it: two steps rather than one pipe, more than one
# source, and a checksum before anything is extracted.
#
# These were `curl ... | tar xJf -` against a single host. That cannot be
# retried -- curl's --retry only covers a request that fails before any data
# arrives, and once the stream is underway a stall leaves tar holding a partial
# archive with nothing to resume from -- and it cannot survive the host having
# a bad day. Both happened during one release: five truncated transfers ("tar:
# short read") across three machines, and then an outright HTTP error from
# gitlab.freedesktop.org, which serves fine from a laptop and throttles CI.
#
# So: download to a file, so --retry means something and a stall trips
# --speed-time instead of hanging; try each mirror in turn; and refuse anything
# whose checksum does not match, which is what makes a second source safe to
# use at all. The sums were taken by fetching from both hosts and comparing.
#
# Kept to options both images have: EL8 ships curl 7.61, which predates
# --retry-all-errors, and busybox mktemp takes no template with a suffix after
# the Xs. Each of those broke a build here before being written down.
fetch_and_unpack() {
  local sha="$1" dest="$2"; shift 2
  local archive url
  archive="$(mktemp)"

  for url in "$@"; do
    echo "fetching $url"
    if curl -sfL --retry 5 --retry-delay 2 \
            --speed-limit 1024 --speed-time 30 \
            -o "$archive" "$url"; then
      if echo "$sha  $archive" | sha256sum -c - > /dev/null 2>&1; then
        tar xJf "$archive" -C "$dest"
        rm -f "$archive"
        return 0
      fi
      echo "checksum mismatch from $url, trying the next source" >&2
    else
      echo "fetch failed from $url, trying the next source" >&2
    fi
  done

  rm -f "$archive"
  echo "every source failed for a tarball with sha256 $sha" >&2
  return 1
}

# install an up-to-date version of meson
python3 -m venv /opt/venv
export PATH="/opt/venv/bin:$PATH"
pip install meson

# compile dummy freetype lib (meant to mirror the api surface of skia's embedded copy via the custom modules.cfg)
FREETYPE=freetype-2.14.1
FREETYPE_SHA256=32427e8c471ac095853212a37aef816c60b42052d4d9e48230bab3bdf2936ccc
FREETYPE_URL=https://sourceforge.net/projects/freetype/files/freetype2/2.14.1/${FREETYPE}.tar.xz/download
FREETYPE_URL_MIRROR=https://download.savannah.gnu.org/releases/freetype/${FREETYPE}.tar.xz
FREETYPE_CFG=/opt/freetype.cfg
fetch_and_unpack "$FREETYPE_SHA256" /opt "$FREETYPE_URL" "$FREETYPE_URL_MIRROR"
cd /opt/${FREETYPE} && \
   cp $FREETYPE_CFG modules.cfg && \
   make && make install

# compile fontconfig (look for config in system dirs but install to /usr/local so we can extract the static lib)
FONTCONFIG_VERSION=2.17.1
FONTCONFIG=fontconfig-$FONTCONFIG_VERSION
FONTCONFIG_SHA256=9f5cae93f4fffc1fbc05ae99cdfc708cd60dfd6612ffc0512827025c026fa541
FONTCONFIG_URL=https://gitlab.freedesktop.org/api/v4/projects/890/packages/generic/fontconfig/$FONTCONFIG_VERSION/${FONTCONFIG}.tar.xz
FONTCONFIG_URL_MIRROR=https://ftp.osuosl.org/pub/blfs/conglomeration/fontconfig/${FONTCONFIG}.tar.xz
fetch_and_unpack "$FONTCONFIG_SHA256" /opt "$FONTCONFIG_URL" "$FONTCONFIG_URL_MIRROR"
cd /opt/${FONTCONFIG} && \
    meson setup -Dprefix=/ -Dsysconfdir=/etc -Dlocalstatedir=/var -Ddefault_library=static -Dprefer_static=true -Dxml-backend=expat -Dtests=disabled -Dtools=disabled --wrap-mode=nofallback build && \
    meson compile -C build && \
    meson install --destdir=/usr/local -C build
