#!/usr/bin/env bash
# Owner-operated offline activation from a separately reviewed, root-staged
# wheelhouse and exact HF snapshot. Never invoked by Core or at service start.
set -euo pipefail
prepare_installer_input() {
    local source=$1 destination=$2 reader_group=$3 unsafe wheel_count bytes
    [[ -d $source && ! -L $source && -d $source/wheels && ! -L $source/wheels ]] ||
        fail 'unsafe reviewed installer source'
    [[ -f $source/requirements.lock && ! -L $source/requirements.lock ]] ||
        fail 'missing reviewed requirements.lock'
    unsafe=$(find "$source/wheels" -mindepth 1 \( -type l -o ! -type f -o ! -name '*.whl' \) -print -quit)
    [[ -z $unsafe ]] || fail "unsafe wheelhouse entry: $unsafe"
    wheel_count=$(find "$source/wheels" -type f -printf '.\n' | wc -l)
    bytes=$(du -sb -- "$source/wheels" "$source/requirements.lock" | awk '{sum += $1} END {print sum+0}')
    (( wheel_count >= 1 && wheel_count <= 512 && bytes <= 17179869184 )) ||
        fail 'reviewed installer input exceeds count or size bounds'
    cp -a -- "$source/requirements.lock" "$source/wheels" "$destination/"
    chown -R "$(id -u):$reader_group" "$destination"
    find "$destination" -type d -exec chmod 0750 {} +
    find "$destination" -type f -exec chmod 0640 {} +
}
fail() { echo "Laya provisioning: $*" >&2; exit 1; }
validate_lockfile() {
    python3 - "$1" <<'PY' || fail 'reviewed lockfile contains an unsupported requirement or source'
import pathlib
import re
import sys

raw = pathlib.Path(sys.argv[1]).read_bytes()
if not raw or len(raw) > 1_048_576:
    raise SystemExit(1)
try:
    lines = raw.decode('ascii').splitlines()
except UnicodeDecodeError:
    raise SystemExit(1)
pin = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]*(?:\[[A-Za-z0-9,._-]+\])?==[0-9][A-Za-z0-9.!+_-]*\Z')
digest = re.compile(r'--hash=sha256:[0-9a-f]{64}\Z')
pending = ''
seen = set()
for line in lines:
    line = line.strip()
    if not line or line.startswith('#'):
        if pending:
            raise SystemExit(1)
        continue
    continuation = line.endswith('\\')
    pending += ' ' + (line[:-1].strip() if continuation else line)
    if continuation:
        continue
    fields = pending.split()
    pending = ''
    if len(fields) < 2 or not pin.fullmatch(fields[0]) or not all(digest.fullmatch(value) for value in fields[1:]):
        raise SystemExit(1)
    name = re.split(r'\[|==', fields[0], maxsplit=1)[0].lower().replace('_', '-').replace('.', '-')
    if name in seen:
        raise SystemExit(1)
    seen.add(name)
if pending or not seen:
    raise SystemExit(1)
PY
}
validate_wheelhouse_hashes() {
    local source=$1 package digest
    while IFS= read -r -d '' package; do
        digest=$(sha256sum -- "$package")
        digest=${digest%% *}
        grep -Fq -- "sha256:$digest" "$source/requirements.lock" ||
            fail "reviewed wheel is not hash-bound by requirements.lock: ${package##*/}"
    done < <(find "$source/wheels" -mindepth 1 -maxdepth 1 -type f -name '*.whl' -print0)
}
run_networkless_as() {
    local identity=$1
    shift
    # Only the trusted namespace setup and privilege-drop helpers run as root.
    # A failed unshare aborts provisioning before any third-party code executes.
    if [[ ${GITHUB_ACTIONS:-} == true && ${JARVIS_LAYA_TEST_ISOLATION_FAIL:-false} == true ]]; then
        return 1
    fi
    /usr/bin/unshare --net -- /usr/sbin/runuser -u "$identity" -- \
        /usr/bin/env -i PATH=/usr/bin:/bin HOME=/nonexistent \
        PIP_NO_CACHE_DIR=1 PYTHONDONTWRITEBYTECODE=1 "$@"
}
# CI exercises the exact materialization/cleanup path with fixture-sized inputs.
# This unprivileged mode cannot provision or activate a runtime.
if [[ ${1:-} == --fixture-installer-input ]]; then
    [[ $# == 3 && ${GITHUB_ACTIONS:-} == true && $EUID != 0 && $3 == /tmp/* && -d $3 && ! -L $3 ]] ||
        fail 'unsafe installer fixture invocation'
    fixture_input=$(mktemp -d "$3/.installer-input.XXXXXXXX")
    trap 'rm -rf -- "$fixture_input"' EXIT
    validate_lockfile "$2/requirements.lock"
    prepare_installer_input "$2" "$fixture_input" "$(id -gn)"
    validate_wheelhouse_hashes "$2"
    [[ -f $fixture_input/requirements.lock && -d $fixture_input/wheels ]] || fail 'fixture copy failed'
    fail 'simulated failure after temporary installer input preparation'
fi
if [[ ${1:-} == --fixture-networkless ]]; then
    [[ $# == 4 && ${GITHUB_ACTIONS:-} == true && $EUID == 0 && $2 == /tmp/* && \
       -d $2 && ! -L $2 && $3 =~ ^[0-9]{1,5}$ && $4 =~ ^[0-9]{1,5}$ && \
       -f $2/probe.py && ! -L $2/probe.py ]] || fail 'unsafe networkless fixture invocation'
    run_networkless_as nobody /usr/bin/python3 "$2/probe.py" "$2" "$3" "$4"
    exit
fi
[[ $EUID == 0 ]] || { echo 'Laya provisioning requires root' >&2; exit 1; }
readonly revision=55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851
readonly wheel_sha=6039e802fa5effb8dd492061cd7ad39a43087beadc4a4fa4a649614e77eb83d4
readonly stage=/var/cache/jarvis-laya/staging
readonly root=/opt/jarvis/laya
readonly models=/var/lib/jarvis-laya/models
readonly wheel=$stage/wheels/laya-0.3.20-py3-none-any.whl
readonly snapshot=$stage/models/$revision

for directory in /var/cache/jarvis-laya "$stage" "$stage/wheels" "$stage/models" "$snapshot"; do
    [[ -d $directory && ! -L $directory ]] || fail "missing or unsafe staging directory: $directory"
    [[ $(stat -c '%u:%g' "$directory") == 0:0 ]] || fail "staging directory is not root:root: $directory"
    mode=$(stat -c '%a' "$directory")
    (( (8#$mode & 0022) == 0 )) || fail "staging directory is writable by non-owner: $directory"
done
[[ -f $stage/requirements.lock && ! -L $stage/requirements.lock ]] || fail 'reviewed requirements.lock is missing'
[[ -f $stage/models.sha256 && ! -L $stage/models.sha256 ]] || fail 'reviewed models.sha256 is missing'
LC_ALL=C awk '
    NF != 2 || $1 !~ /^[0-9a-f]{64}$/ || $2 !~ /^[A-Za-z0-9][A-Za-z0-9._/-]*$/ ||
    $2 ~ /(^|\/)\.\.?(\/|$)/ || $2 ~ /^\// || $2 ~ /\/\// || $2 ~ /\/$/ { exit 1 }
    seen[$2]++ { exit 1 }
    END { if (NR == 0) exit 1 }
' "$stage/models.sha256" || fail 'model checksum manifest is malformed or unsafe'
[[ -f $wheel && ! -L $wheel ]] || fail 'pinned Laya wheel is missing'
[[ $(sha256sum "$wheel" | awk '{print $1}') == "$wheel_sha" ]] || fail 'Laya wheel hash mismatch'
grep -Eq '^laya\[serve\]==0[.]3[.]20([[:space:]\\]|$)' "$stage/requirements.lock" ||
    fail 'lockfile does not pin laya[serve]==0.3.20'
grep -Fq "sha256:$wheel_sha" "$stage/requirements.lock" || fail 'lockfile omits reviewed Laya wheel hash'
validate_lockfile "$stage/requirements.lock"
unsafe=$(find "$stage" \( -type l -o \( -type f -o -type d \) \( -perm /022 -o ! -user root -o ! -group root \) -o ! -type f -a ! -type d \) -print -quit)
[[ -z $unsafe ]] || fail "unsafe staged file or symlink: $unsafe"
validate_wheelhouse_hashes "$stage"
unsafe=$(find "$snapshot" -type f ! \( -name '*.json' -o -name '*.safetensors' -o -name '*.txt' -o -name '*.model' \) -print -quit)
[[ -z $unsafe ]] || fail "unexpected model file: $unsafe"
diff -u \
    <(find "$snapshot" -type f -printf '%P\n' | LC_ALL=C sort) \
    <(awk '{print $2}' "$stage/models.sha256" | LC_ALL=C sort) >/dev/null ||
    fail 'model snapshot contains an unlisted or missing file'
(cd "$snapshot" && sha256sum --check --strict "$stage/models.sha256" >/dev/null) || fail 'model hashes do not match reviewed manifest'
[[ -f $snapshot/model.safetensors && -f $snapshot/multilingual/model.safetensors ]] || fail 'both pinned checkpoints are required'
for weights in "$snapshot/model.safetensors" "$snapshot/multilingual/model.safetensors"; do
    [[ $(stat -c '%s' "$weights") -ge 100000000 ]] || fail "model weights are implausibly small: $weights"
done

if ! getent group jarvis-laya >/dev/null; then
    groupadd --system jarvis-laya
fi
if ! getent passwd jarvis-laya >/dev/null; then
    useradd --system --gid jarvis-laya --home-dir /nonexistent --shell /usr/sbin/nologin jarvis-laya
fi
[[ $(id -gn jarvis-laya) == jarvis-laya ]] || fail 'Laya service identity has an unexpected group'
[[ $(id -u jarvis-laya) -ne 0 ]] || fail 'Laya service identity must be unprivileged'
for directory in /opt /opt/jarvis "$root" "$root/releases" /var/lib /var/lib/jarvis-laya "$models"; do
    [[ -e $directory || -L $directory ]] || continue
    [[ -d $directory && ! -L $directory ]] || fail "runtime parent is not a real directory: $directory"
    metadata=$(stat -c '%u:%a' "$directory")
    [[ ${metadata%%:*} == 0 ]] && (( (8#${metadata##*:} & 0022) == 0 )) ||
        fail "runtime parent has unsafe ownership or permissions: $directory"
done
install -d -o root -g root -m 0755 "$root" "$root/releases"
install -d -o root -g jarvis-laya -m 0750 /var/lib/jarvis-laya "$models"
candidate=$(mktemp -d "$root/releases/.laya-0.3.20.XXXXXXXX")
model_candidate=$(mktemp -d "$models/.snapshot.XXXXXXXX")
installer_input=$(mktemp -d "$root/releases/.installer-input.XXXXXXXX")
cleanup() {
    [[ -z ${candidate:-} || ! -d $candidate ]] || rm -rf -- "$candidate"
    [[ -z ${model_candidate:-} || ! -d $model_candidate ]] || rm -rf -- "$model_candidate"
    [[ -z ${installer_input:-} || ! -d $installer_input ]] || rm -rf -- "$installer_input"
}
trap cleanup EXIT
python3 -m venv "$candidate"
# Never mutate canonical reviewed root:root staging. Only this disposable copy
# is readable by jarvis-laya; pip still runs offline and requires lockfile hashes.
prepare_installer_input "$stage" "$installer_input" jarvis-laya
chown -R jarvis-laya:jarvis-laya "$candidate"
run_networkless_as jarvis-laya \
    "$candidate/bin/python" -m pip install --disable-pip-version-check --no-index \
    --find-links "$installer_input/wheels" --require-hashes --only-binary=:all: \
    -r "$installer_input/requirements.lock" >/dev/null
[[ $(runuser -u jarvis-laya -- env -i PATH=/usr/bin:/bin HOME=/nonexistent \
    "$candidate/bin/python" -c 'import importlib.metadata; print(importlib.metadata.version("laya"))') == 0.3.20 ]] ||
    fail 'installed Laya version differs from reviewed wheel'
chown -R root:jarvis-laya "$candidate"
find "$candidate" -type d -exec chmod 0750 {} +
find "$candidate" -type f -perm /111 -exec chmod 0750 {} +
find "$candidate" -type f ! -perm /111 -exec chmod 0640 {} +
cp -a -- "$snapshot/." "$model_candidate/"
cp -- "$stage/models.sha256" "$model_candidate/models.sha256"
chown -R root:jarvis-laya "$model_candidate"
find "$model_candidate" -type d -exec chmod 0750 {} +
find "$model_candidate" -type f -exec chmod 0640 {} +
(cd "$model_candidate" && sha256sum --check --strict models.sha256 >/dev/null) || fail 'copied model hashes mismatch'
if [[ ! -e $models/$revision && ! -L $models/$revision ]]; then
    mv -T -- "$model_candidate" "$models/$revision"
    model_candidate=
else
    [[ -d $models/$revision && ! -L $models/$revision ]] || fail 'existing model snapshot path is unsafe'
    (cd "$models/$revision" && sha256sum --check --strict models.sha256 >/dev/null) ||
        fail 'existing pinned snapshot differs from its manifest'
fi
final="$root/releases/laya-0.3.20-$revision"
[[ ! -e $final && ! -L $final ]] || fail 'pinned runtime already exists; refusing overwrite'
mv -T -- "$candidate" "$final"
candidate=
if [[ -e $root/current || -L $root/current ]]; then
    [[ -L $root/current ]] || fail 'active Laya runtime is not a managed symlink'
    active=$(readlink -f -- "$root/current") || fail 'active Laya runtime link is broken'
    [[ $active == "$root/releases/"* ]] || fail 'active Laya runtime escapes managed releases'
fi
[[ ! -e $root/.current-new && ! -L $root/.current-new ]] || fail 'stale Laya activation staging link exists'
ln -s "releases/${final##*/}" "$root/.current-new"
mv -Tf -- "$root/.current-new" "$root/current"
echo 'Pinned Laya runtime provisioned; service remains disabled until owner acceptance.'
