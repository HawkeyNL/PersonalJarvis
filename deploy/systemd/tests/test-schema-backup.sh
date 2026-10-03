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
# The free-space pre-flight runs before any service is stopped.
mkdir -p "$fixture/lowspace"
printf '#!/usr/bin/env bash\nprintf "Filesystem 1024-blocks Used Available Capacity Mounted on\\nfixture 1000 1000 0 100%%%% /\\n"\n' > "$fixture/lowspace/df"
printf '#!/usr/bin/env bash\ntouch "%s/lowspace/systemctl-called"\nexit 1\n' "$fixture" > "$fixture/lowspace/systemctl"
chmod 0755 "$fixture/lowspace/"*
if PATH="$fixture/lowspace:$PATH" bash "$helper" create v1.0.0 v1.0.1 2>"$fixture/lowspace.err"; then
    echo 'snapshot started without enough free space' >&2; exit 1
fi
grep -Fq 'insufficient space' "$fixture/lowspace.err"
[[ ! -e $fixture/lowspace/systemctl-called ]]
[[ -z $(find "$fixture/state/migration-backups" -mindepth 1 -print -quit) ]]
# Database and backups share a filesystem here, so restore space counts
# twice: 1 GiB fits once with margin (1.2 GiB free) but not twice.
printf '#!/usr/bin/env bash\nprintf "1000000\\t%%s\\n" "$3"\n' > "$fixture/lowspace/du"
printf '#!/usr/bin/env bash\nprintf "Filesystem 1024-blocks Used Available Capacity Mounted on\\nfixture 9000000 7800000 1200000 87%%%% /\\n"\n' > "$fixture/lowspace/df"
chmod 0755 "$fixture/lowspace/du" "$fixture/lowspace/df"
if PATH="$fixture/lowspace:$PATH" bash "$helper" create v1.0.0 v1.0.1 2>"$fixture/lowspace.err"; then
    echo 'snapshot ignored restore space on a shared filesystem' >&2; exit 1
fi
grep -Fq 'need 2114 MiB' "$fixture/lowspace.err"
[[ ! -e $fixture/lowspace/systemctl-called ]]
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
