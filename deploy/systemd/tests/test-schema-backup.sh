#!/usr/bin/env bash
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'root-owned snapshot fixture requires root' >&2; exit 1; }
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/bin" "$fixture/state/surrealdb"
chmod 0700 "$fixture/state" "$fixture/state/surrealdb"
printf 'original fixture database\n' > "$fixture/state/surrealdb/CURRENT"
printf '#!/usr/bin/env bash\n[[ $1 != is-active ]]\n' > "$fixture/bin/systemctl"
printf '#!/usr/bin/env bash\n[[ ${FIXTURE_RUNNING:-false} != true ]] || echo running\n' > "$fixture/bin/docker"
chmod 0755 "$fixture/bin/"*
export PATH="$fixture/bin:$PATH" GITHUB_ACTIONS=true JARVIS_SCHEMA_TEST_MODE=true JARVIS_SCHEMA_FIXTURE_ROOT="$fixture/state"
helper="$repo/deploy/systemd/schema-backup.sh"
if FIXTURE_RUNNING=true bash "$helper" create v1.0.0 v1.0.1; then echo 'running database was copied' >&2; exit 1; fi
id=$(bash "$helper" create v1.0.0 v1.0.1)
[[ $id =~ ^txn\.[A-Za-z0-9]{8}$ ]]
[[ $(stat -c '%u:%g:%a' "$fixture/state/migration-backups/$id") == 0:0:700 ]]
printf 'failed candidate fixture\n' > "$fixture/state/surrealdb/CURRENT"
bash "$helper" restore "$id"
cmp <(printf 'original fixture database\n') "$fixture/state/surrealdb/CURRENT"
cmp <(printf 'failed candidate fixture\n') "$fixture/state/.surrealdb-failed-$id/CURRENT"

id=$(bash "$helper" create v1.0.0 v1.0.1)
bash "$helper" commit "$id"
if bash "$helper" restore "$id"; then echo 'post-commit security rewind allowed' >&2; exit 1; fi

id=$(bash "$helper" create v1.0.0 v1.0.1)
printf 'tamper\n' >> "$fixture/state/migration-backups/$id/database/CURRENT"
if bash "$helper" restore "$id"; then echo 'tampered backup restored' >&2; exit 1; fi
cmp <(printf 'original fixture database\n') "$fixture/state/surrealdb/CURRENT"
ln -s /dev/null "$fixture/state/surrealdb/unsafe"
if bash "$helper" create v1.0.0 v1.0.1; then echo 'symlink database accepted' >&2; exit 1; fi
if bash "$helper" restore ../escape; then echo 'arbitrary snapshot path accepted' >&2; exit 1; fi
echo 'Cold snapshot verification, recovery, tamper and post-commit refusal tests passed'
