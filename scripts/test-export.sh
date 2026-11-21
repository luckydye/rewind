#!/usr/bin/env bash
# Exports every format (PNG, MP4, GIF) plain, cropped, annotated and both from
# a running rewindd, and checks the files: pixel size, frame count, GIF timing
# and looping, and that the drawing lands where it was drawn, the right way up,
# inside the crop.
#
# Needs a running daemon (make build; daemon/.build/debug/rewindd), ffmpeg,
# and a screen ffplay can draw on. Usage: scripts/test-export.sh [rewindctl]
set -euo pipefail

CTL=${1:-daemon/.build/debug/rewindctl}
OUT=$(mktemp -d /tmp/rewind-export.XXXXXX)
FROM=6
TO=1
CROP=0.25,0.2,0.5,0.5
failures=0

pass() { printf '  ok    %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*"; failures=$((failures + 1)); }

read -r WIDTH HEIGHT < <("$CTL" status --json | python3 -c 'import json,sys; s=json.load(sys.stdin); print(s["width"], s["height"])')
echo "capture ${WIDTH}x${HEIGHT}, output in $OUT"

# Opaque markers in full-frame fractions: blue around (0.30, 0.25), yellow
# around (0.70, 0.65). Inside the crop they sit at (0.1, 0.1) and (0.9, 0.9).
python3 - "$OUT/overlay.rgba" "$WIDTH" "$HEIGHT" <<'EOF'
import struct, sys
path, w, h = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
pixels = bytearray(w * h * 4)
size = int(w * 0.04)
for (fx, fy), colour in (((0.30, 0.25), (0, 0, 255)), ((0.70, 0.65), (255, 255, 0))):
    cx, cy = int(fx * w), int(fy * h)
    row = bytes(colour + (255,)) * (2 * size)
    for y in range(cy - size, cy + size):
        start = (y * w + cx - size) * 4
        pixels[start:start + len(row)] = row
with open(path, "wb") as f:
    f.write(struct.pack(">II", w, h))
    f.write(pixels)
EOF

ffplay -loglevel quiet -noborder -autoexit -t 120 -f lavfi -i testsrc2=size=1400x900:rate=30 &
PLAYER=$!
trap 'kill $PLAYER 2>/dev/null || true' EXIT
sleep $((FROM + 2))

# Colour at a fractional position of the first frame, as "r g b".
sample() {
    ffmpeg -loglevel error -i "$1" -frames:v 1 -f rawvideo -pix_fmt rgb24 - | python3 -c '
import sys
w, h, fx, fy = int(sys.argv[1]), int(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
data = sys.stdin.buffer.read()
at = (int(fy * h) * w + int(fx * w)) * 3
print(*data[at:at + 3])' "$2" "$3" "$4" "$5"
}

near() { # near "r g b" r g b
    python3 -c 'import sys; a=list(map(int,sys.argv[1].split())); b=list(map(int,sys.argv[2:]))
sys.exit(0 if max(abs(x-y) for x,y in zip(a,b)) < 70 else 1)' "$@"
}

check_markers() { # file width height cropped
    local file=$1 w=$2 h=$3 cropped=$4 bx=0.30 by=0.25 yx=0.70 yy=0.65
    [[ $cropped == yes ]] && { bx=0.1; by=0.1; yx=0.9; yy=0.9; }
    local blue yellow
    blue=$(sample "$file" "$w" "$h" "$bx" "$by")
    yellow=$(sample "$file" "$w" "$h" "$yx" "$yy")
    if near "$blue" 0 0 255 && near "$yellow" 255 255 0; then
        pass "drawing in place (blue $blue, yellow $yellow)"
    else
        fail "drawing misplaced: blue at ($bx,$by) is $blue, yellow at ($yx,$yy) is $yellow"
    fi
}

# Every delay is at least one 1/fps slot (to the centisecond GIF stores) and
# the average rate does not exceed fps.
check_gif_timing() { # file fps
    local summary
    if summary=$(ffprobe -v error -select_streams v:0 -show_entries frame=duration_time -of csv=p=0 "$1" | python3 -c '
import math, sys
fps = float(sys.argv[1])
delays = [float(line) for line in sys.stdin if line.strip()]
average = len(delays) / sum(delays)
print(f"{len(delays)} frames, {sum(delays):.2f}s, {average:.1f} fps average, delays {min(delays):.2f}-{max(delays):.2f}s")
sys.exit(1 if min(delays) < math.floor(100 / fps) / 100 - 1e-6 or average > fps + 0.3 else 0)' "$2"); then
        pass "timing: $summary"
    else
        fail "timing at $2 fps: $summary"
    fi
}

expected_size() { # format cropped -> "w h"
    python3 -c '
import sys
w, h, fmt, cropped = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4] == "yes"
if cropped:
    w, h = round(0.5 * w), round(0.5 * h)
    w -= w % 2; h -= h % 2
if fmt == "gif" and w > 1280:
    h, w = int(h * 1280 / w), 1280
print(w, h)' "$WIDTH" "$HEIGHT" "$1" "$2"
}

for format in png mp4 gif; do
    for variant in plain crop overlay both; do
        echo "$format / $variant"
        file="$OUT/$variant.$format"
        args=(export --format "$format" --out "$file")
        [[ $format == png ]] && args+=(--at 1) || args+=(--from $FROM --to $TO)
        [[ $variant == crop || $variant == both ]] && args+=(--crop $CROP) && cropped=yes || cropped=no
        [[ $variant == overlay || $variant == both ]] && args+=(--overlay "$OUT/overlay.rgba")

        if ! report=$("$CTL" "${args[@]}" 2>&1); then
            fail "export: $report"
            continue
        fi
        pass "$report"

        read -r want_w want_h < <(expected_size "$format" "$cropped")
        IFS=x read -r got_w got_h < <(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0:s=x "$file")
        [[ "$got_w $got_h" == "$want_w $want_h" ]] && pass "${got_w}x${got_h}" || fail "size ${got_w}x${got_h}, expected ${want_w}x${want_h}"

        if [[ $format != png ]]; then
            frames=$(ffprobe -v error -count_frames -select_streams v:0 -show_entries stream=nb_read_frames -of csv=p=0 "$file")
            duration=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file")
            python3 -c 'import sys; sys.exit(0 if abs(float(sys.argv[1]) - float(sys.argv[2])) < 1.5 else 1)' "$duration" $((FROM - TO)) \
                && pass "$frames frames, ${duration}s" || fail "duration ${duration}s, expected ~$((FROM - TO))s"
        fi

        if [[ $format == gif ]]; then
            grep -q NETSCAPE2.0 "$file" && pass "loops" || fail "no NETSCAPE2.0 loop extension"
            check_gif_timing "$file" 15
        fi

        [[ $variant == overlay || $variant == both ]] && check_markers "$file" "$got_w" "$got_h" "$cropped"
    done
done

echo "gif / options --gif-fps 8 --gif-width 640"
"$CTL" export --format gif --from $FROM --to $TO --gif-fps 8 --gif-width 640 --out "$OUT/options.gif" >/dev/null
IFS=x read -r got_w _ < <(ffprobe -v error -show_entries stream=width,height -of csv=p=0:s=x "$OUT/options.gif")
[[ $got_w == 640 ]] && pass "640 wide" || fail "width $got_w, expected 640"
check_gif_timing "$OUT/options.gif" 8

# Copy renders to Caches and puts the file itself on the pasteboard, which is
# what Finder, Messages and Slack paste. Replaces the clipboard.
for format in mp4 gif; do
    echo "copy $format / crop + drawing"
    if ! report=$("$CTL" copy --format "$format" --from $FROM --to $TO --crop $CROP --overlay "$OUT/overlay.rgba" 2>&1); then
        fail "copy: $report"
        continue
    fi
    pass "$report"
    pasted=$(osascript -e 'POSIX path of (the clipboard as «class furl»)' 2>&1) || true
    if [[ $pasted == *.$format && -f $pasted ]]; then
        pass "clipboard holds $(basename "$pasted")"
        IFS=x read -r got_w got_h < <(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0:s=x "$pasted")
        check_markers "$pasted" "$got_w" "$got_h" yes
    else
        fail "clipboard holds '$pasted', expected a .$format file"
    fi
done

echo
# The exports are of the real screen, so they are only kept to inspect a failure.
(( failures == 0 )) && { rm -rf "$OUT"; echo "all passed"; } || { echo "$failures failed — files in $OUT"; exit 1; }
