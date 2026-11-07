#!/usr/bin/env bash
# Shell sample: a small backup script exercising most constructs.
set -euo pipefail

SRC="${1:-$HOME/notes}"
DEST="${2:-/tmp/backup}"
readonly STAMP="$(date +%Y%m%d-%H%M%S)"

log() {
    printf '[%s] %s\n' "$(date +%T)" "$*" >&2
}

usage() {
    cat <<EOF
usage: $(basename "$0") [source] [dest]
  source  directory to back up (default: ~/notes)
  dest    where archives go     (default: /tmp/backup)
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
    usage
    exit 0
fi

[[ -d "$SRC" ]] || { log "no such directory: $SRC"; exit 1; }
mkdir -p "$DEST"

count=0
for f in "$SRC"/*; do
    [[ -f "$f" ]] || continue
    count=$((count + 1))
done

case "$count" in
    0) log "nothing to back up"; exit 0 ;;
    1) log "one file" ;;
    *) log "$count files" ;;
esac

archive="$DEST/notes-$STAMP.tar.gz"
tar -czf "$archive" -C "$(dirname "$SRC")" "$(basename "$SRC")"
log "wrote $archive ($(du -h "$archive" | cut -f1))"
