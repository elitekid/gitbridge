#!/usr/bin/env bash
# 릴리스용 update.txt 와 서명 update.txt.sig 를 만든다.
# 사용법: make-update.sh <키.pem> <GitBridge.exe> <버전> <출력폴더>
# 버전은 저장소 Cargo.toml 의 version 과 같아야 하고,
# 키의 공개키는 crates/gitbridge/src/gui/update.rs 의 PUBLIC_KEY 와 같아야 한다.
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "사용법: make-update.sh <키.pem> <GitBridge.exe> <버전> <출력폴더>" >&2
    exit 2
fi

KEY="$1"
EXE="$2"
VERSION="$3"
OUT="$4"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "버전은 X.Y.Z 형식이어야 합니다." >&2
    exit 2
fi
CARGO_VERSION="$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
if [ "$VERSION" != "$CARGO_VERSION" ]; then
    echo "버전 $VERSION 이 저장소 Cargo.toml 의 $CARGO_VERSION 과 다릅니다." >&2
    exit 2
fi
if [ ! -f "$KEY" ]; then
    echo "키 파일을 찾을 수 없습니다." >&2
    exit 2
fi
if [ ! -f "$EXE" ]; then
    echo "exe 파일을 찾을 수 없습니다." >&2
    exit 2
fi

CODE_HEX="$(sed -n '/^const PUBLIC_KEY/,/^];/p' "$ROOT/crates/gitbridge/src/gui/update.rs" | grep -o '0x[0-9a-fA-F][0-9a-fA-F]' | sed 's/^0x//' | tr 'A-F' 'a-f' | tr -d '\n')"
if [ "${#CODE_HEX}" != "64" ]; then
    echo "update.rs 에서 PUBLIC_KEY 32바이트를 읽지 못했습니다." >&2
    exit 1
fi

if [ -x /mingw64/bin/openssl ]; then
    OPENSSL=/mingw64/bin/openssl
else
    OPENSSL="$(command -v openssl || true)"
fi
if [ -z "$OPENSSL" ]; then
    echo "openssl 을 찾을 수 없습니다." >&2
    exit 1
fi

TMP_PUB="$(mktemp)"
TMP_ERR="$(mktemp)"
TMP_DER="$(mktemp)"
trap 'rm -f "$TMP_PUB" "$TMP_ERR" "$TMP_DER"' EXIT

# openssl 을 돌리고, 실패하면 그 오류를 보여 준다.
run_openssl() {
    local what="$1"
    shift
    if ! "$OPENSSL" "$@" 2>"$TMP_ERR"; then
        echo "$what" >&2
        cat "$TMP_ERR" >&2
        exit 1
    fi
}

run_openssl "공개키를 만들지 못했습니다." pkey -in "$KEY" -pubout -out "$TMP_PUB"
run_openssl "공개키를 읽지 못했습니다." pkey -pubin -in "$TMP_PUB" -outform DER -out "$TMP_DER"
PUB_HEX="$(tail -c 32 "$TMP_DER" | od -An -tx1 | tr -d ' \n')"
if [ "$PUB_HEX" != "$CODE_HEX" ]; then
    echo "키의 공개키가 update.rs 의 PUBLIC_KEY 와 다릅니다. 다른 키로 서명하면 앱이 받아들이지 않습니다." >&2
    exit 1
fi

mkdir -p "$OUT"
cp "$EXE" "$OUT/GitBridge.exe"

SHA="$(sha256sum "$OUT/GitBridge.exe" | cut -d ' ' -f 1 | tr 'A-F' 'a-f')"
if ! [[ "$SHA" =~ ^[0-9a-f]{64}$ ]]; then
    echo "sha256 을 계산하지 못했습니다." >&2
    exit 1
fi

printf 'product=GitBridge\nversion=%s\nsha256=%s\n' "$VERSION" "$SHA" > "$OUT/update.txt"

run_openssl "서명에 실패했습니다." pkeyutl -sign -inkey "$KEY" -rawin -in "$OUT/update.txt" -out "$OUT/update.txt.sig"

SIZE="$(wc -c < "$OUT/update.txt.sig" | tr -d ' ')"
if [ "$SIZE" != "64" ]; then
    echo "서명 파일이 64바이트가 아닙니다." >&2
    exit 1
fi

if ! "$OPENSSL" pkeyutl -verify -pubin -inkey "$TMP_PUB" -rawin -in "$OUT/update.txt" -sigfile "$OUT/update.txt.sig" >/dev/null 2>"$TMP_ERR"; then
    echo "자체 검증에 실패했습니다." >&2
    cat "$TMP_ERR" >&2
    exit 1
fi
echo "자체 검증을 통과했고, 공개키가 update.rs 의 PUBLIC_KEY 와 같습니다."

echo "만든 파일(출력 폴더): GitBridge.exe, update.txt, update.txt.sig"
echo "버전: $VERSION"
echo "sha256: $SHA"
