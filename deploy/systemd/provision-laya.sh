#!/usr/bin/env bash
# Owner-operated offline activation from a separately reviewed, root-staged
# wheelhouse and exact HF snapshot. Never invoked by Core or at service start.
set -euo pipefail
[[ $EUID == 0 ]] || { echo 'Laya provisioning requires root' >&2; exit 1; }
readonly revision=55cf4c4ebb4ebe31b2550e8bdf3bd21b99753851
readonly wheel_sha=6039e802fa5effb8dd492061cd7ad39a43087beadc4a4fa4a649614e77eb83d4
readonly stage=/var/cache/jarvis-laya/staging
readonly root=/opt/jarvis/laya
readonly models=/var/lib/jarvis-laya/models
readonly wheel=$stage/wheels/laya-0.3.20-py3-none-any.whl
readonly snapshot=$stage/models/$revision
fail() { echo "Laya provisioning: $*" >&2; exit 1; }

for directory in /var/cache/jarvis-laya "$stage" "$stage/wheels" "$stage/models" "$snapshot"; do
    [[ -d $directory && ! -L $directory ]] || fail "missing or unsafe staging directory: $directory"
    [[ $(stat -c '%u' "$directory") == 0 ]] || fail "staging directory is not root-owned: $directory"
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
unsafe=$(find "$stage" \( -type l -o -type f \( -perm /022 -o ! -user root -o ! -group root \) \) -print -quit)
[[ -z $unsafe ]] || fail "unsafe staged file or symlink: $unsafe"
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
cleanup() {
    [[ -z ${candidate:-} || ! -d $candidate ]] || rm -rf -- "$candidate"
    [[ -z ${model_candidate:-} || ! -d $model_candidate ]] || rm -rf -- "$model_candidate"
}
trap cleanup EXIT
python3 -m venv "$candidate"
# The wheelhouse is read-only to the service identity. Execute third-party
# installer code without root authority or an inherited privileged environment.
chgrp jarvis-laya /var/cache/jarvis-laya
chmod 0750 /var/cache/jarvis-laya
chown -R root:jarvis-laya "$stage"
find "$stage" -type d -exec chmod 0750 {} +
find "$stage" -type f -exec chmod 0640 {} +
chown -R jarvis-laya:jarvis-laya "$candidate"
runuser -u jarvis-laya -- env -i PATH=/usr/bin:/bin HOME=/nonexistent \
    PIP_NO_CACHE_DIR=1 PYTHONDONTWRITEBYTECODE=1 \
    "$candidate/bin/python" -m pip install --disable-pip-version-check --no-index \
    --find-links "$stage/wheels" --require-hashes -r "$stage/requirements.lock" >/dev/null
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
