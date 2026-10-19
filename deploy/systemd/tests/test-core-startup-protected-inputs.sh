#!/usr/bin/env bash
# Root-only CI regression: reproduce the protected Home Node inputs as seen by
# the unprivileged service.  This catches unreadable directory traversal, not
# merely permissive file modes.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || { echo "CI root fixture only" >&2; exit 1; }
api_bin=${JARVIS_API_BINARY:?set JARVIS_API_BINARY to the built jarvis-api}
[[ -x $api_bin ]] || { echo "jarvis-api binary is unavailable" >&2; exit 1; }
# This fixture owns fixed production-shaped paths only on an empty CI host.
# Refuse before registering cleanup or creating anything on an installed host.
for path in /etc/jarvis /var/lib/jarvis/agents /run/jarvis-core-admin /run/jarvis-core; do
    [[ ! -e $path && ! -L $path ]] || { echo "refusing existing host path: $path" >&2; exit 1; }
done

fixture=$(mktemp -d)
chmod 0755 "$fixture"
install -d -o root -g root -m 0755 "$fixture/bin"
install -o root -g root -m 0755 "$api_bin" "$fixture/bin/jarvis-api"
api_bin="$fixture/bin/jarvis-api"
pid=
created_user=false
created_caddy=false
cleanup() {
    if [[ -n ${pid:-} ]]; then kill "$pid" 2>/dev/null || true; fi
    wait "${pid:-}" 2>/dev/null || true
    rm -rf -- "$fixture" /etc/jarvis /var/lib/jarvis/agents /run/jarvis-core-admin /run/jarvis-core
    if [[ ${created_user:-false} == true ]]; then userdel jarvis 2>/dev/null || true; fi
    if [[ ${created_caddy:-false} == true ]]; then userdel caddy 2>/dev/null || true; fi
}
trap cleanup EXIT

if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin jarvis
    created_user=true
fi
if ! getent passwd caddy >/dev/null; then
    useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin caddy
    created_caddy=true
fi
# runuser does not run systemd's RuntimeDirectory setup. Reproduce the exact
# unit contract, rather than weakening Core's required ownership/mode checks.
install -d -o jarvis -g jarvis -m 0700 /run/jarvis-core-admin
install -d -o jarvis -g caddy -m 2750 /run/jarvis-core
install -d -o root -g jarvis -m 0750 /etc/jarvis
install -o root -g jarvis -m 0640 /dev/stdin /etc/jarvis/Jarvis.md <<'EOF'
# Jarvis

Synthetic protected persona.
EOF
install -o root -g root -m 0600 /dev/null /etc/jarvis/surrealdb.env
install -d -o root -g jarvis -m 0750 /etc/jarvis/model-policy
printf '%s' '{"version":1,"paid_api":"off"}' \
    | install -o root -g jarvis -m 0640 /dev/stdin /etc/jarvis/model-policy/routing.json
install -d -o root -g jarvis -m 0750 /var/lib/jarvis/agents/releases
bundle=/var/lib/jarvis/agents/releases/bundle-ci-protected-inputs
install -d -o root -g jarvis -m 0750 "$bundle/agents"
agent_json='{"id":"ci-agent","name":"CI Agent","description":"Synthetic fixture","model_policy":"default","instructions":"Synthetic instructions.","requested_capabilities":[],"allowed_tools":[],"denied_actions":[],"limits":{"max_runtime_seconds":30,"max_context_chars":1000,"max_output_chars":500,"max_parallel_runs":1}}'
printf '%s' "$agent_json" | install -o root -g jarvis -m 0640 /dev/stdin "$bundle/agents/ci-agent.json"
agent_hash=$(sha256sum "$bundle/agents/ci-agent.json" | awk '{print $1}')
printf '{"version":1,"bundle_id":"bundle-ci-protected-inputs","agents":[{"id":"ci-agent","path":"agents/ci-agent.json","sha256":"%s"}]}' "$agent_hash" \
    | install -o root -g jarvis -m 0640 /dev/stdin "$bundle/manifest.json"
ln -s releases/bundle-ci-protected-inputs /var/lib/jarvis/agents/current

runuser -u jarvis -- test -r /etc/jarvis/Jarvis.md
runuser -u jarvis -- test -r /var/lib/jarvis/agents/current/manifest.json
runuser -u jarvis -- test ! -w /etc/jarvis/Jarvis.md
runuser -u jarvis -- test ! -w /var/lib/jarvis/agents/current/manifest.json
runuser -u jarvis -- test ! -r /etc/jarvis/surrealdb.env
runuser -u jarvis -- test -r /etc/jarvis/model-policy/routing.json
runuser -u jarvis -- test ! -w /etc/jarvis/model-policy/routing.json

# Match production's database-scoped Core identity rather than granting this
# fixture the provisioning-only root account.
printf "DEFINE USER core ON DATABASE PASSWORD 'ci-core-password' ROLES EDITOR;\n" \
    | docker exec -i jarvis-surreal-test /surreal sql --hide-welcome \
        --endpoint ws://127.0.0.1:8000 --username root --password test-root-password \
        --auth-level root --namespace jarvis --database core >/dev/null

core_env=(
    JARVIS_ENVIRONMENT=production JARVIS_BIND_ADDR=127.0.0.1:18080
    JARVIS_API_UNIX_SOCKET=/run/jarvis-core/api.sock
    JARVIS_SURREAL_ENDPOINT=127.0.0.1:8000 JARVIS_SURREAL_NAMESPACE=jarvis
    JARVIS_SURREAL_DATABASE=core JARVIS_SURREAL_USERNAME=core
    JARVIS_SURREAL_PASSWORD=ci-core-password JARVIS_LLM_PROVIDER=ollama
    JARVIS_LLM_PERSONA_PATH=/etc/jarvis/Jarvis.md
    JARVIS_AGENT_BUNDLE_PATH=/var/lib/jarvis/agents/current
)

# Production Core must refuse an invalid persona without echoing its text.
printf 'SYNTHETIC-PERSONA-BODY-7f3a\n' \
    | install -o root -g jarvis -m 0640 /dev/stdin /etc/jarvis/Jarvis-invalid.md
status=0
runuser -u jarvis -- env "${core_env[@]}" JARVIS_LLM_PERSONA_PATH=/etc/jarvis/Jarvis-invalid.md \
    timeout 20 "$api_bin" >"$fixture/invalid-persona.log" 2>&1 || status=$?
# A timeout (124) means Core kept running, for example on a fallback persona.
if ((status == 0 || status == 124)); then
    echo "Core did not refuse an invalid persona (exit $status)" >&2
    exit 1
fi
# Only the production refusal says "is unusable"; the fallback warning does not.
grep -Fq 'is unusable: persona must start with' "$fixture/invalid-persona.log"
if grep -Fq 'SYNTHETIC-PERSONA-BODY' "$fixture/invalid-persona.log"; then
    echo "Core logged persona text" >&2
    exit 1
fi

runuser -u jarvis -- env "${core_env[@]}" "$api_bin" >"$fixture/api.log" 2>&1 &
pid=$!
for _attempt in $(seq 1 20); do
    if curl --fail --silent --max-time 1 http://127.0.0.1:18080/livez >/dev/null \
        && curl --fail --silent --max-time 1 http://127.0.0.1:18080/readyz >/dev/null; then
        break
    fi
    kill -0 "$pid" 2>/dev/null || { cat "$fixture/api.log" >&2; exit 1; }
    sleep 1
done
curl --fail --silent http://127.0.0.1:18080/livez >/dev/null
curl --fail --silent http://127.0.0.1:18080/readyz >/dev/null
# Production TCP answers health probes only; the API lives on the socket.
[[ $(curl --silent --output /dev/null --write-out '%{http_code}' http://127.0.0.1:18080/) == 404 ]]
[[ -S /run/jarvis-core/api.sock && ! -L /run/jarvis-core/api.sock ]]
[[ $(stat -c '%U:%G:%a' /run/jarvis-core/api.sock) == jarvis:caddy:660 ]]
[[ $(curl --silent --output /dev/null --write-out '%{http_code}' --unix-socket /run/jarvis-core/api.sock http://core/) == 200 ]]
grep -Fq 'Jarvis persona loaded' "$fixture/api.log"
grep -Fq 'private AgentRegistry loaded' "$fixture/api.log"
grep -Fq 'model routing loaded' "$fixture/api.log"
[[ -S /run/jarvis-core-admin/devices.sock && ! -L /run/jarvis-core-admin/devices.sock ]]
[[ $(stat -c '%U:%a' /run/jarvis-core-admin/devices.sock) == jarvis:600 ]]
python3 - <<'PY'
import json
import socket
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
    client.settimeout(5)
    client.connect('/run/jarvis-core-admin/devices.sock')
    client.sendall(b'{"operation":"list"}\n')
    response = json.loads(client.makefile('rb').readline(262145))
    assert response == {"ok": True, "data": {"devices": []}}, response
PY
# The service account owns the socket but does not acquire administrator
# authority merely by being able to connect to it.
runuser -u jarvis -- python3 - <<'PY'
import socket
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
    client.settimeout(5)
    client.connect('/run/jarvis-core-admin/devices.sock')
    try:
        client.sendall(b'{"operation":"list"}\n')
        assert client.recv(1024) == b'', 'non-root peer received an authorized response'
    except (BrokenPipeError, ConnectionResetError):
        pass
PY
echo "Protected-input Core startup fixture passed"
