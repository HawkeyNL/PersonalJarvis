#!/usr/bin/env bash
# Root-only CI regression: reproduce the protected Home Node inputs as seen by
# the unprivileged service.  This catches unreadable directory traversal, not
# merely permissive file modes.
set -euo pipefail
[[ ${GITHUB_ACTIONS:-} == true && ${EUID} -eq 0 ]] || { echo "CI root fixture only" >&2; exit 1; }
api_bin=${JARVIS_API_BINARY:?set JARVIS_API_BINARY to the built jarvis-api}
[[ -x $api_bin ]] || { echo "jarvis-api binary is unavailable" >&2; exit 1; }

fixture=$(mktemp -d)
pid=
created_user=false
cleanup() {
    if [[ -n ${pid:-} ]]; then kill "$pid" 2>/dev/null || true; fi
    wait "${pid:-}" 2>/dev/null || true
    rm -rf -- "$fixture" /etc/jarvis /var/lib/jarvis/agents
    if [[ ${created_user:-false} == true ]]; then userdel jarvis 2>/dev/null || true; fi
}
trap cleanup EXIT

if ! getent passwd jarvis >/dev/null; then
    useradd --system --user-group --home-dir /nonexistent --shell /usr/sbin/nologin jarvis
    created_user=true
fi
install -d -o root -g jarvis -m 0750 /etc/jarvis
install -o root -g jarvis -m 0640 /dev/stdin /etc/jarvis/Jarvis.md <<'EOF'
Synthetic protected persona.
EOF
install -o root -g root -m 0600 /dev/null /etc/jarvis/surrealdb.env
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

runuser -u jarvis -- env \
    JARVIS_ENVIRONMENT=production JARVIS_BIND_ADDR=127.0.0.1:18080 \
    JARVIS_SURREAL_ENDPOINT=127.0.0.1:8000 JARVIS_SURREAL_NAMESPACE=jarvis \
    JARVIS_SURREAL_DATABASE=core JARVIS_SURREAL_USERNAME=root \
    JARVIS_SURREAL_PASSWORD=test-root-password JARVIS_LLM_PROVIDER=ollama \
    JARVIS_LLM_PERSONA_PATH=/etc/jarvis/Jarvis.md \
    JARVIS_AGENT_BUNDLE_PATH=/var/lib/jarvis/agents/current \
    "$api_bin" >"$fixture/api.log" 2>&1 &
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
grep -Fq 'Jarvis persona loaded' "$fixture/api.log"
grep -Fq 'private AgentRegistry loaded' "$fixture/api.log"
echo "Protected-input Core startup fixture passed"
