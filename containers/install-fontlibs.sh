#!/usr/bin/env bash
set -euxo pipefail

export CC=clang
export CXX=clang++

# Fetch a tarball and unpack it, as two steps rather than one pipe.
#
# These were `curl ... | tar xJf -`, which cannot be retried: curl's --retry
# only covers a request that fails before any data arrives, and once the
# stream has started, a stall leaves tar with a partial archive and nothing to
# resume from. It failed five times across three machines during one release,
# always the same way -- `tar: short read` -- and always fatally, because the
# image build is where it happens.
#
# Downloading to a file lets curl retry properly, gives it a stall timeout
# rather than an indefinite hang, and lets the archive be checked before
# anything is extracted from it.
fetch_and_unpack() {
  local url="$1" dest="$2" archive
  archive="$(mktemp /tmp/fontlib-XXXXXX.tar.xz)"
  curl -sfL --retry 5 --retry-delay 2 --retry-all-errors \
       --speed-limit 1024 --speed-time 30 \
       -o "$archive" "$url"
  tar tJf "$archive" > /dev/null
  tar xJf "$archive" -C "$dest"
  rm -f "$archive"
}

# install an up-to-date version of meson
python3 -m venv /opt/venv
export PATH="/opt/venv/bin:$PATH"
pip install meson

# compile dummy freetype lib (meant to mirror the api surface of skia's embedded copy via the custom modules.cfg)
FREETYPE=freetype-2.14.1
FREETYPE_URL=https://sourceforge.net/projects/freetype/files/freetype2/2.14.1/${FREETYPE}.tar.xz/download
FREETYPE_CFG=/opt/freetype.cfg
fetch_and_unpack "$FREETYPE_URL" /opt
cd /opt/${FREETYPE} && \
   cp $FREETYPE_CFG modules.cfg && \
   make && make install

# compile fontconfig (look for config in system dirs but install to /usr/local so we can extract the static lib)
FONTCONFIG_VERSION=2.17.1
FONTCONFIG=fontconfig-$FONTCONFIG_VERSION
FONTCONFIG_URL=https://gitlab.freedesktop.org/api/v4/projects/890/packages/generic/fontconfig/$FONTCONFIG_VERSION/${FONTCONFIG}.tar.xz
fetch_and_unpack "$FONTCONFIG_URL" /opt
cd /opt/${FONTCONFIG} && \
    meson setup -Dprefix=/ -Dsysconfdir=/etc -Dlocalstatedir=/var -Ddefault_library=static -Dprefer_static=true -Dxml-backend=expat -Dtests=disabled -Dtools=disabled --wrap-mode=nofallback build && \
    meson compile -C build && \
    meson install --destdir=/usr/local -C build
