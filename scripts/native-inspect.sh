#!/usr/bin/env bash
# Checks one native mod's owner update with tools/native-subset (.github/workflows/owner-update.yml's `inspect` and
# replay). Takes no token and needs none: both packages are downloaded by their public release URLs into a new
# directory, rehashed, and the checker runs with an empty environment, so a package that exploits it finds nothing
# to use.
#
#   native-inspect.sh <checker> <id> <version> <old url> <old sha256> <new url> <new sha256> <pins>
#
# <pins>: "<horizon commit>=<horizon-mixins commit or ->" pairs, space-separated (owner_update.py's `pins`). GH_REPO
# names the repository whose releases the URLs must be. Prints the checker's JSON (or a fixed one when a package's
# SHA-256 is not its entry's) and exits 0 when the update is in the subset, 1 when it is refused, 3 when it couldn't
# be checked.
set -uo pipefail
if [ $# -ne 8 ]; then
  echo "usage: $0 <checker> <id> <version> <old url> <old sha256> <new url> <new sha256> <pins>" >&2
  exit 3
fi
checker="$1" id="$2" version="$3" old_url="$4" old_sha="$5" new_url="$6" new_sha="$7" pins="$8"
release="https://github.com/${GH_REPO:?}/releases/download/"
part='[A-Za-z0-9_+-][A-Za-z0-9._+-]{0,127}'
for u in "$old_url" "$new_url"; do
  rest="${u#"$release"}"
  if [ "$rest" = "$u" ] || [[ ! "$rest" =~ ^$part/$part$ ]]; then
    echo "not a release download of $GH_REPO: $u" >&2
    exit 3
  fi
done
pin='[0-9a-f]{40}=([0-9a-f]{40}|-)'
if [[ ! "$id" =~ ^[a-z0-9_]{1,64}$ || ! "$version" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ \
  || ! "$old_sha" =~ ^[0-9a-f]{64}$ || ! "$new_sha" =~ ^[0-9a-f]{64}$ || ! "$pins" =~ ^$pin( $pin)*$ ]]; then
  echo "malformed id, version, SHA-256 or pins" >&2
  exit 3
fi
dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
for side in old new; do
  url="${side}_url" sha="${side}_sha"
  if ! curl -fsSL --proto '=https' --proto-redir '=https' --max-filesize $((64 << 20)) --max-time 300 --retry 2 \
    -o "$dir/$side.hmod" "${!url}"; then
    echo "couldn't download the $side package" >&2
    exit 3
  fi
  if ! printf '%s  %s\n' "${!sha}" "$dir/$side.hmod" | sha256sum --check --status; then
    echo '{"ok":false,"rules":["R1"],"findings":[],"dropped":0}'
    exit 1
  fi
done
args=()
for p in $pins; do args+=(--pin "$p"); done
status=0
# Bounded in time and memory; the packages are at most 64 MiB each.
( ulimit -v $((4 << 20)); exec env -i PATH=/usr/bin:/bin timeout 300 "$checker" --id "$id" --version "$version" \
    --old "$dir/old.hmod" --old-sha256 "$old_sha" --new "$dir/new.hmod" --new-sha256 "$new_sha" "${args[@]}" \
) > "$dir/result.json" || status=$?
case $status in
  0 | 1) cat "$dir/result.json"; exit $status ;;
  *) echo "native-subset failed ($status)" >&2; exit 3 ;;
esac
