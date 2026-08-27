#!/usr/bin/env bash
# Run provisioning against the digest-pinned official image. The image is
# intentionally shell-less, so this is a regression test for direct /surreal
# execution as well as secret-safe Docker invocation.
set -euo pipefail

[[ ${EUID} -eq 0 ]] || {
    echo "shell-less provisioning test must run as root" >&2
    exit 1
}
command -v docker >/dev/null 2>&1 || {
    echo "shell-less provisioning test requires Docker" >&2
    exit 1
}

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
subject_source="$repo_dir/deploy/surrealdb/provision-core-user.sh"
real_docker=$(command -v docker)
tmp=$(mktemp -d /run/jarvis-shellless-provision.XXXXXX)
compose_file="$tmp/docker-compose.yml"
env_file="$tmp/surrealdb.env"
marker="$tmp/provisioned"
password_file="$tmp/core-password"
output_file="$tmp/provision-output"
argv_file="$tmp/docker-argv"
bin_dir="$tmp/bin"
test_core_password='Q2xlYW5Sb290QWNjZXNzQ29yZVBhc3N3b3JkVGhhdElzTm90UmVhbDEyMzQ1Njc4OTA='
test_root_password='root-password-only-for-shellless-regression'

cleanup() {
    "$real_docker" compose --env-file "$env_file" -f "$compose_file" down --volumes --remove-orphans >/dev/null 2>&1 || true
    rm -rf -- "$tmp"
}
trap cleanup EXIT
mkdir -p "$bin_dir" "$tmp/data"

install -o root -g root -m 0600 /dev/null "$env_file"
cat > "$env_file" <<EOF
SURREALDB_IMAGE=surrealdb/surrealdb@sha256:d653f6c8a89e81f865ee31cd2f587c50f50ace922175e04150b1e385d2f86011
SURREAL_ROOT_USER=root
SURREAL_ROOT_PASSWORD=$test_root_password
EOF

# Keep the production service definition, but make its name and data directory
# test-local and remove its host port mapping to avoid host-service collisions.
sed \
    -e "s/^name: jarvis-surrealdb$/name: jarvis-shellless-provision/" \
    -e "s#- /var/lib/jarvis/surrealdb:/data#- $tmp/data:/data#" \
    -e '/^    ports:/,/^    volumes:/ { /^    volumes:/!d }' \
    "$repo_dir/deploy/surrealdb/docker-compose.yml" > "$compose_file"

# Substitute fixed production paths only in a disposable copy.
sed \
    -e "s|^readonly env_file=/etc/jarvis/surrealdb.env$|readonly env_file=$env_file|" \
    -e "s|^readonly marker=/etc/jarvis/surrealdb-core-provisioned$|readonly marker=$marker|" \
    "$subject_source" > "$tmp/provision-core-user.sh"
chmod 0700 "$tmp/provision-core-user.sh"

cat > "$bin_dir/openssl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$JARVIS_TEST_CORE_PASSWORD"
EOF
chmod 0700 "$bin_dir/openssl"

cat > "$bin_dir/docker" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\0' "$@" > "$JARVIS_TEST_DOCKER_ARGV"
for argument in "$@"; do
    case "$argument" in
        *"$JARVIS_TEST_ROOT_PASSWORD"*|*"$JARVIS_TEST_CORE_PASSWORD"*)
            echo "secret appeared in Docker argv" >&2
            exit 98
            ;;
    esac
done
exec "$JARVIS_REAL_DOCKER" "$@"
EOF
chmod 0700 "$bin_dir/docker"

assert_absent() {
    local needle=$1
    local file=$2
    if grep -Fq "$needle" "$file"; then
        echo "unexpected value found in $file" >&2
        exit 1
    fi
}

assert_absent_nul() {
    local needle=$1
    local file=$2
    if grep -Fzxq "$needle" "$file"; then
        echo "unexpected argument found in $file" >&2
        exit 1
    fi
}

"$real_docker" compose --env-file "$env_file" -f "$compose_file" up -d --wait

# The observed production failure must remain a failure: the image has no sh.
if "$real_docker" compose --env-file "$env_file" -f "$compose_file" exec -T surrealdb sh </dev/null >/dev/null 2>&1; then
    echo "official SurrealDB test image unexpectedly provides sh" >&2
    exit 1
fi

PATH="$bin_dir:$PATH" \
JARVIS_REAL_DOCKER="$real_docker" \
JARVIS_TEST_CORE_PASSWORD="$test_core_password" \
JARVIS_TEST_ROOT_PASSWORD="$test_root_password" \
JARVIS_TEST_DOCKER_ARGV="$argv_file" \
JARVIS_SURREALDB_COMPOSE_FILE="$compose_file" \
    bash "$tmp/provision-core-user.sh" --password-file "$password_file" > "$output_file"

[[ $(stat -c '%U:%G:%a' "$password_file") == root:root:600 ]]
[[ $(stat -c '%U:%G:%a' "$marker") == root:root:600 ]]
[[ $(<"$password_file") == "$test_core_password" ]]
assert_absent "$test_root_password" "$output_file"
assert_absent "$test_core_password" "$output_file"
assert_absent_nul sh "$argv_file"
assert_absent_nul bash "$argv_file"
grep -Fzq /surreal "$argv_file"
grep -Fzq sql "$argv_file"

# The root credential inherited by the running container must make the direct
# SQL client usable; no container shell is involved in this verification.
printf 'INFO FOR DB;\n' \
    | "$real_docker" compose --env-file "$env_file" -f "$compose_file" exec -T surrealdb \
        /surreal sql --hide-welcome --endpoint ws://127.0.0.1:8000 \
            --auth-level root --namespace jarvis --database core > /dev/null

# The marker prevents a second account creation or password rotation.
second_password_file="$tmp/core-password-second"
PATH="$bin_dir:$PATH" \
JARVIS_REAL_DOCKER="$real_docker" \
JARVIS_TEST_CORE_PASSWORD="$test_core_password" \
JARVIS_TEST_ROOT_PASSWORD="$test_root_password" \
JARVIS_TEST_DOCKER_ARGV="$argv_file" \
JARVIS_SURREALDB_COMPOSE_FILE="$compose_file" \
    bash "$tmp/provision-core-user.sh" --password-file "$second_password_file" >> "$output_file"
[[ ! -e $second_password_file ]]
grep -Fq 'existing Core user retained; no credentials rotated' "$output_file"

echo "Shell-less SurrealDB Core-user provisioning checks passed"
