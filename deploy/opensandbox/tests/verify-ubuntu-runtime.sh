#!/usr/bin/env bash
# Verify the OpenSandbox runtime boundary on the Ubuntu Home Node.
#
# This is a root-operated verification aid, not a deployment or firewall
# script. It creates nothing, opens no ports, and never prints the API key.
# Run it only after an intentionally created disposable sandbox exists:
#   sudo JARVIS_OPENSANDBOX_VERIFY_RUNTIME=1 \
#     deploy/opensandbox/tests/verify-ubuntu-runtime.sh
#
# It deliberately fails when a check cannot be proven. Disk quotas and the
# actual in-workload egress probes remain explicit operator checks because the
# reviewed Docker runtime does not expose a portable per-container rootfs quota.
set -euo pipefail

fail() {
  echo "OpenSandbox verification failed: $*" >&2
  exit 1
}

[[ ${EUID} -eq 0 ]] || fail "run as root"
[[ ${JARVIS_OPENSANDBOX_VERIFY_RUNTIME:-} == 1 ]] || fail "set JARVIS_OPENSANDBOX_VERIFY_RUNTIME=1"
command -v docker >/dev/null || fail "docker is required"
command -v curl >/dev/null || fail "curl is required"
command -v jq >/dev/null || fail "jq is required"

systemctl is-active --quiet jarvis-opensandbox.service || fail "jarvis-opensandbox is not active"

# Health is intentionally unauthenticated and minimal. Every lifecycle/proxy
# route must reject a missing API key after the local authentication patch.
curl --fail --silent --show-error http://127.0.0.1:8090/health >/dev/null
unauth_status=$(curl --silent --output /dev/null --write-out '%{http_code}' \
  http://127.0.0.1:8090/v1/sandboxes)
[[ $unauth_status == 401 || $unauth_status == 403 ]] || fail "lifecycle API accepted no API key ($unauth_status)"

# Neither control plane nor transient workload proxy ports may bind beyond the
# local host. The regex accepts IPv4 and IPv6 loopback listeners only.
while IFS= read -r address; do
  case "$address" in
    127.0.0.1:8090|\[::1\]:8090|127.0.0.1:410*|\[::1\]:410*) ;;
    *) fail "unexpected public/non-loopback sandbox listener: $address" ;;
  esac
done < <(ss -ltnH | awk '$4 ~ /:(8090|410[0-9][0-9])$/ { print $4 }')

# Jarvis Core must stay a separate, unprivileged native systemd process and
# must not inherit the Docker socket used by the trusted control plane.
[[ $(systemctl show -p User --value jarvis-core.service) == jarvis ]] || fail "jarvis-core is not the jarvis user"
core_pid=$(systemctl show -p MainPID --value jarvis-core.service)
[[ $core_pid =~ ^[1-9][0-9]*$ ]] || fail "jarvis-core has no running main PID"
[[ ! -e /proc/$core_pid/root/var/run/docker.sock ]] || fail "jarvis-core can see Docker socket"

# Workloads and their egress sidecars are identified by upstream's stable labels.
mapfile -t workload_ids < <(
  {
    docker ps -q --filter 'label=opensandbox.io/id'
    docker ps -q --filter 'label=opensandbox.io/egress-sidecar-for'
  } | sort -u
)
[[ ${#workload_ids[@]} -gt 0 ]] || fail "create one disposable OpenSandbox workload before verification"

for container_id in "${workload_ids[@]}"; do
  inspect=$(docker inspect "$container_id")
  privileged=$(jq -r '.[0].HostConfig.Privileged' <<<"$inspect")
  network_mode=$(jq -r '.[0].HostConfig.NetworkMode' <<<"$inspect")
  pids_limit=$(jq -r '.[0].HostConfig.PidsLimit' <<<"$inspect")
  memory_limit=$(jq -r '.[0].HostConfig.Memory' <<<"$inspect")
  nano_cpus=$(jq -r '.[0].HostConfig.NanoCpus' <<<"$inspect")
  no_new_privileges=$(jq -r '.[0].HostConfig.SecurityOpt // [] | index("no-new-privileges:true") != null' <<<"$inspect")
  binds=$(jq -r '.[0].HostConfig.Binds // [] | .[]?' <<<"$inspect")
  cap_add=$(jq -r '.[0].HostConfig.CapAdd // [] | .[]?' <<<"$inspect")

  [[ $privileged == false ]] || fail "$container_id is privileged"
  [[ $network_mode != host ]] || fail "$container_id uses host networking"
  [[ $pids_limit =~ ^[1-9][0-9]*$ ]] || fail "$container_id lacks a PID limit"
  [[ $memory_limit =~ ^[1-9][0-9]*$ ]] || fail "$container_id lacks a memory limit"
  [[ $nano_cpus =~ ^[1-9][0-9]*$ ]] || fail "$container_id lacks a CPU limit"
  [[ $no_new_privileges == true ]] || fail "$container_id lacks no-new-privileges"
  [[ $binds != *'/var/run/docker.sock'* ]] || fail "$container_id receives Docker socket"
  [[ $binds != *'/var/lib/jarvis'* && $binds != *'/etc/jarvis'* && $binds != *'/home'* ]] || fail "$container_id receives a protected host path"
  [[ $cap_add != *SYS_ADMIN* ]] || fail "$container_id adds CAP_SYS_ADMIN"
done

cat <<'EOF'
Static runtime boundary checks passed.

Before activation, also record these adversarial checks against this exact
patched image and a disposable profile-bound task:
  1. `curl 127.0.0.1:8080`, `curl 169.254.169.254`, and scans of the owner LAN
     time out/fail from the workload; an allowed public domain succeeds only
     when its profile permits it.
  2. `/var/run/docker.sock`, `/var/lib/jarvis`, `/etc/jarvis`, and the host's
     SurrealDB files are absent from the workload filesystem.
  3. Timeout, cancellation, output cap, artifact traversal rejection and
     post-task deletion all work through the authenticated manager API.
  4. `docker inspect` proves the selected Kata runtime; Docker/runc is not
     sufficient for production Codex/browser work.
  5. Record a root-operated proof of the storage quota for the selected runtime.

Do not activate the Core/Codex sandbox broker until all five checks are retained
with the deployed image digests and Home Node OS version.
EOF
