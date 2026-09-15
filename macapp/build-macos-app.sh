#!/bin/bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
CACHE="$ROOT/.macos-build-cache"
DOWNLOADS="$CACHE/downloads"
STAGE="$CACHE/stage"
DERIVED="$CACHE/DerivedData"
DIST="$ROOT/dist"
DEVELOPER_DIR=${DEVELOPER_DIR:-/Applications/Xcode-beta.app/Contents/Developer}
FFMPEG_VERSION=8.1.2
MKVTOOLNIX_VERSION=100.0
MEDIAINFO_VERSION=26.05
export DEVELOPER_DIR
export CARGO_HOME="$CACHE/cargo"
export RUSTUP_HOME="$CACHE/rustup"
export PATH="$CARGO_HOME/bin:/usr/bin:/bin:/usr/sbin:/sbin"

if [[ $(uname -m) != arm64 ]]; then
  echo "This build must run on an Apple Silicon Mac." >&2
  exit 1
fi

mkdir -p "$DOWNLOADS" "$STAGE" "$DIST"

download() {
  local url=$1 out=$2 checksum=$3
  if [[ ! -s $out ]]; then
    curl --fail --location --retry 3 --output "$out" "$url"
  fi
  printf '%s  %s\n' "$checksum" "$out" | shasum -a 256 -c -
}

if [[ ! -x "$CARGO_HOME/bin/cargo" ]]; then
  download "https://static.rust-lang.org/rustup/dist/aarch64-apple-darwin/rustup-init" "$DOWNLOADS/rustup-init" ec1b9233e7f72990ecd8e62063fa7f6c3dfc2bec8e97f88bff165f9100ac696a
  chmod +x "$DOWNLOADS/rustup-init"
  "$DOWNLOADS/rustup-init" -y --no-modify-path --profile minimal --default-toolchain 1.98.1
fi

export RUSTUP_TOOLCHAIN=1.98.1
rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal
export RUSTFLAGS="-C target-cpu=apple-m1"
cargo build --locked --release --manifest-path "$ROOT/dv8_converter/Cargo.toml"
cargo build --locked --release --manifest-path "$ROOT/dv8_converter/Cargo.toml" --example l5_timeline
cargo build --locked --release --manifest-path "$ROOT/dovi_tool/Cargo.toml" --no-default-features --features internal-font

FFMPEG_KEY=$(/usr/bin/shasum -a 256 "$ROOT/macapp/build-macos-app.sh" | cut -c1-20)

FFMPEG_ARCHIVE="$DOWNLOADS/ffmpeg-$FFMPEG_VERSION.tar.xz"
FFMPEG_SOURCE="$CACHE/ffmpeg-$FFMPEG_VERSION"
FFMPEG_PREFIX="$STAGE/ffmpeg-$FFMPEG_KEY"
download "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" "$FFMPEG_ARCHIVE" 464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
if [[ ! -f "$FFMPEG_PREFIX/.complete" || ! -x "$FFMPEG_PREFIX/bin/ffmpeg" || ! -x "$FFMPEG_PREFIX/bin/ffprobe" ]]; then
  rm -rf "$FFMPEG_SOURCE" "$FFMPEG_PREFIX"
  tar -xf "$FFMPEG_ARCHIVE" -C "$CACHE"
  cd "$FFMPEG_SOURCE"
  ./configure \
    --prefix="$FFMPEG_PREFIX" \
    --arch=arm64 \
    --cc="$(xcrun --find clang)" \
    --host-cc="$(xcrun --find clang)" \
    --host-cflags="-isysroot $(xcrun --sdk macosx --show-sdk-path)" \
    --host-ld="$(xcrun --find clang)" \
    --host-ldflags="-isysroot $(xcrun --sdk macosx --show-sdk-path)" \
    --sysroot="$(xcrun --sdk macosx --show-sdk-path)" \
    --enable-gpl --enable-version3 \
    --disable-doc --disable-debug --disable-network --disable-shared \
    --disable-everything \
    --enable-ffmpeg --enable-ffprobe --enable-avfilter --enable-swscale --enable-videotoolbox \
    --enable-demuxer=matroska,mov,rawvideo --enable-decoder=hevc,rawvideo --enable-parser=hevc \
    --enable-hwaccel=hevc_videotoolbox \
    --enable-protocol=file,pipe --enable-muxer=null,rawvideo,framehash --enable-encoder=wrapped_avframe,rawvideo \
    --enable-filter=crop,cropdetect,format,hwdownload,metadata,null,scale,scdet,signalstats,showinfo,select,setparams
  make -j"$(sysctl -n hw.logicalcpu)"
  make install
  touch "$FFMPEG_PREFIX/.complete"
fi

MKV_DMG="$DOWNLOADS/MKVToolNix-$MKVTOOLNIX_VERSION-arm64.dmg"
MEDIAINFO_DMG="$DOWNLOADS/MediaInfo_CLI_$MEDIAINFO_VERSION.dmg"
download "https://mkvtoolnix.download/macos/releases/$MKVTOOLNIX_VERSION/MKVToolNix-$MKVTOOLNIX_VERSION-1-arm64.dmg" "$MKV_DMG" 155ded045a35bd1079ba466c2e1011bdcb1a85b74c984ed5627841cd313850a0
download "https://mediaarea.net/download/binary/mediainfo/$MEDIAINFO_VERSION/MediaInfo_CLI_${MEDIAINFO_VERSION}_Mac.dmg" "$MEDIAINFO_DMG" 507605a7c8f1054a6996d99a4ef5b5a0711cfbf2f8ca2ef5161d6ee701ea8015

MKV_MOUNT="$CACHE/mnt-mkv"
MEDIAINFO_MOUNT="$CACHE/mnt-mediainfo"
mkdir -p "$MKV_MOUNT" "$MEDIAINFO_MOUNT"
hdiutil attach -quiet -readonly -nobrowse -mountpoint "$MKV_MOUNT" "$MKV_DMG"
hdiutil attach -quiet -readonly -nobrowse -mountpoint "$MEDIAINFO_MOUNT" "$MEDIAINFO_DMG"
cleanup_mounts() {
  hdiutil detach -quiet "$MKV_MOUNT" 2>/dev/null || true
  hdiutil detach -quiet "$MEDIAINFO_MOUNT" 2>/dev/null || true
}
trap cleanup_mounts EXIT

MEDIAINFO_EXPANDED="$STAGE/mediainfo-pkg"
rm -rf "$MEDIAINFO_EXPANDED"
pkgutil --expand-full "$MEDIAINFO_MOUNT/mediainfo.pkg" "$MEDIAINFO_EXPANDED"
MEDIAINFO_BIN=$(find "$MEDIAINFO_EXPANDED" -type f -path '*/usr/local/bin/mediainfo' -print -quit)
if [[ -z $MEDIAINFO_BIN ]]; then
  echo "MediaInfo executable was not found in the official package." >&2
  exit 1
fi

rm -rf "$DERIVED"
xcodebuild -project "$ROOT/macapp/DV8Maker.xcodeproj" -scheme DV8Maker \
  -configuration Release -derivedDataPath "$DERIVED" CODE_SIGNING_ALLOWED=NO build

APP="$DIST/DV8 Maker.app"
rm -rf "$APP"
cp -R "$DERIVED/Build/Products/Release/DV8 Maker.app" "$APP"
RESOURCES="$APP/Contents/Resources"
TOOLS="$RESOURCES/tools"
mkdir -p "$TOOLS" "$RESOURCES/vendor" "$RESOURCES/config" "$RESOURCES/licenses"

cp "$ROOT/dv8_converter/target/release/dv8_converter" "$TOOLS/dv8_converter"
cp "$ROOT/dovi_tool/target/release/dovi_tool" "$TOOLS/dovi_tool"
cp "$FFMPEG_PREFIX/bin/ffmpeg" "$TOOLS/ffmpeg"
cp "$FFMPEG_PREFIX/bin/ffprobe" "$TOOLS/ffprobe"
cp "$MEDIAINFO_BIN" "$TOOLS/mediainfo"
cp "$ROOT/macapp/wrappers/mkvmerge" "$TOOLS/mkvmerge"
cp "$ROOT/macapp/wrappers/mkvextract" "$TOOLS/mkvextract"
cp -R "$MKV_MOUNT/MKVToolNix.app/Contents/MacOS" "$RESOURCES/vendor/MKVToolNix"
cp "$ROOT/config/DV7toDV8.json" "$RESOURCES/config/DV7toDV8.json"

cp "$ROOT/dovi_tool/LICENSE" "$RESOURCES/licenses/dovi_tool-MIT.txt"
cp "$MKV_MOUNT/COPYING.txt" "$RESOURCES/licenses/MKVToolNix-GPL.txt"
cp "$MEDIAINFO_MOUNT/License.html" "$RESOURCES/licenses/MediaInfo-License.html"
cp "$FFMPEG_SOURCE/COPYING.GPLv3" "$RESOURCES/licenses/FFmpeg-GPLv3.txt"
cp "$ROOT/LICENSE" "$RESOURCES/licenses/DV8-MIT.txt"
/usr/bin/python3 "$ROOT/macapp/bundle-rust-licenses.py" "$RESOURCES"
chmod +x "$TOOLS"/* "$RESOURCES/vendor/MKVToolNix"/mkvmerge "$RESOURCES/vendor/MKVToolNix"/mkvextract

for tool in dv8_converter dovi_tool mediainfo; do
  if ! file "$TOOLS/$tool" | grep -q 'arm64'; then
    echo "$tool is not an arm64 executable" >&2
    exit 1
  fi
done

"$TOOLS/dv8_converter" --help >/dev/null
for tool in dovi_tool mediainfo mkvmerge mkvextract; do
  "$TOOLS/$tool" --version >/dev/null
done
"$TOOLS/ffmpeg" -version >/dev/null
"$TOOLS/ffprobe" -version >/dev/null

if find "$APP" -type f -perm -111 -print0 | xargs -0 otool -L 2>/dev/null | grep -q '/opt/homebrew'; then
  echo "App contains a Homebrew runtime dependency." >&2
  exit 1
fi

codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
echo "Built: $APP"
