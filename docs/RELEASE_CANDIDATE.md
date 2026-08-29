# Linux release-candidate build and Home Node acceptance

Jarvis Linux release bytes are built only on the canonical Ubuntu 26.04 x86-64
builder with Rust/Cargo 1.97.1. `rust-toolchain.toml` and the release workflow
pin the toolchain; the release script rejects another OS, toolchain, inherited
Rust flags, malformed tag or abbreviated revision.

The shared builder is usable by a normal local or GitLab runner user:

```bash
bash scripts/release/build-linux.sh stage vMAJOR.MINOR.PATCH "$(git rev-parse HEAD)"
```

It runs one Cargo invocation, stages the resulting executables under
`dist/candidate/`, records their hashes and public compiler provenance, and
does not use sudo. It remaps workspace and Cargo-home paths so those paths do
not make otherwise equivalent release binaries differ.

CI then runs the PTY smoke test as root against the exact staged `jarvis`
bytes:

```bash
sudo -n python3 scripts/ci/test-admin-tui-pty.py \
  dist/candidate/jarvis-core-vMAJOR.MINOR.PATCH/jarvis
```

The smoke test requires a useful first frame, verifies the process remains
alive without an exit key, resizes the PTY, exits with `q`, checks canonical
input/echo, alternate-screen and cursor restoration, and confirms JSON never
starts the TUI. Only after that test should the same staged directory be
packaged:

```bash
bash scripts/release/build-linux.sh package vMAJOR.MINOR.PATCH "$(git rev-parse HEAD)"
```

The GitHub workflow uploads those candidate bytes but its publish job is gated
by the `home-node-release-acceptance` environment. That environment must have
required reviewers and its `HOME_NODE_ACCEPTED_REVISION` variable must equal
the exact 40-character candidate revision. A missing or stale value fails the
publish job closed.

## Real Home Node acceptance

Download and verify the candidate checksum. Extract it under the normal
owner's development directory, review the binary hash, and install only that
binary as `/usr/local/sbin/jarvis-dev`. Production
`/usr/local/sbin/jarvis` remains untouched.

Run the read-only sequence locally and over SSH:

```bash
sudo /usr/local/sbin/jarvis-dev --tui-trace status
sudo /usr/local/sbin/jarvis-dev health
sudo /usr/local/sbin/jarvis-dev update --check
sudo /usr/local/sbin/jarvis-dev update --status
sudo /usr/local/sbin/jarvis-dev models list
sudo /usr/local/sbin/jarvis-dev credentials list
sudo /usr/local/sbin/jarvis-dev agents status
sudo /usr/local/sbin/jarvis-dev logs core --lines 100
```

For persistent views, test narrow and wide terminals, resize, `q`, Esc and
Ctrl-C. Also verify `NO_COLOR`, `TERM=dumb`, redirected stdout, redirected
stdin and every `--json` form use deterministic plain/JSON output without
alternate-screen controls. After every normal exit, error and interrupt,
verify echo, canonical input, cursor and screen state are restored.

Do not run a real update through `jarvis-dev` during read-only acceptance.
Exercise long-running progress and cancellation with safe fixtures first. Do
not set `HOME_NODE_ACCEPTED_REVISION` or approve publication until the exact
candidate passes the local terminal, SSH, fallback and restoration matrix.
