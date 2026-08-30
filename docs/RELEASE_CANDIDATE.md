# Linux release-candidate build and Home Node acceptance

Jarvis Linux release bytes are built only on the canonical Ubuntu 26.04 x86-64
builder with Rust/Cargo 1.97.1 and Node.js 24.20.0. `rust-toolchain.toml` and the
release workflow pin the toolchain; the release script rejects another OS,
toolchain, inherited Rust flags, malformed tag or abbreviated revision.

The shared builder is usable by a normal local or GitLab runner user:

```bash
bash scripts/release/build-linux.sh stage vMAJOR.MINOR.PATCH "$(git rev-parse HEAD)"
```

It builds the server workspace once and the separate Tauri workspace once,
stages the resulting executables under `dist/candidate/`, records their hashes,
three independent component versions and public compiler provenance, and does
not use sudo. It remaps workspace and Cargo-home paths so those paths do not
make otherwise equivalent release binaries differ.

CI then runs the PTY smoke test as root against the exact staged `jarvis`
bytes:

```bash
sudo -n python3 scripts/ci/test-admin-tui-pty.py \
  dist/candidate/jarvis-core-vMAJOR.MINOR.PATCH/jarvis
```

The workflow also runs `jarvis-core-admin --component-version` against the
exact staged GUI executable. Packaging emits a checksummed public component
manifest alongside the Core archive; publication verifies that it exactly
matches `release.json` inside the accepted archive.

The smoke test requires a useful first frame, verifies the process remains
alive without an exit key, resizes the PTY, exits with `q`, checks canonical
input/echo, alternate-screen and cursor restoration, and confirms JSON never
starts the TUI. Only after that test should the same staged directory be
packaged:

```bash
bash scripts/release/build-linux.sh package vMAJOR.MINOR.PATCH "$(git rev-parse HEAD)"
```

The tag-triggered GitHub workflow stops after uploading those candidate bytes.
It does not publish a release. After the exact artifact passes Home Node
acceptance, run the separate `Publish accepted Jarvis Core candidate` workflow
manually with:

- the existing candidate tag;
- its exact 40-character revision;
- the workflow run ID that built and tested the candidate;
- explicit confirmation that those exact bytes passed Home Node acceptance.

The publication job downloads the immutable artifact from that exact run,
checks the originating tag, revision and successful candidate job, validates
the checksum and packaged manifest, and only then creates the release. It is
also gated by the `home-node-release-acceptance` environment. Configure that
environment with required reviewers so publication needs a distinct human
approval. Invalid, missing or stale inputs fail closed.

## Real Home Node acceptance

Download and verify the candidate checksum. Extract it under the normal
owner's development directory, review the binary hash, and install only that
binary as `/usr/local/sbin/jarvis-dev`. Production
`/usr/local/sbin/jarvis` remains untouched.

Run the read-only sequence locally and over SSH:

```bash
sudo /usr/local/sbin/jarvis-dev
sudo /usr/local/sbin/jarvis-dev --tui-trace status
sudo /usr/local/sbin/jarvis-dev health
sudo /usr/local/sbin/jarvis-dev update --check
sudo /usr/local/sbin/jarvis-dev update --status
sudo /usr/local/sbin/jarvis-dev models list
sudo /usr/local/sbin/jarvis-dev credentials list
sudo /usr/local/sbin/jarvis-dev agents status
sudo /usr/local/sbin/jarvis-dev logs core --lines 100
```

In the bare console, visit Overview, Update, Health, Services, Agents, Models,
Credentials, Logs and System. Confirm that navigation does not enter a second
alternate screen, quick checks keep the application open and result states
remain visible. For persistent views, test narrow and wide terminals, resize,
`q`, Esc and Ctrl-C. Also verify `NO_COLOR`, `TERM=dumb`, redirected stdout,
redirected stdin, bare non-interactive invocation and every `--json` form use
deterministic plain/JSON output without alternate-screen controls. After every
normal exit, error and interrupt, verify echo, canonical input, cursor and
screen state are restored.

Install the candidate Core Admin App package on the GNOME console and verify it
runs as the normal desktop user. It must show one polkit authentication dialog,
reuse that typed broker session while active, lock after five inactive minutes
and request fresh system authentication when unlocked. Confirm Update displays
the current/latest Core, CLI and Core Admin App versions.

Do not run a real update through `jarvis-dev` during read-only acceptance.
Exercise long-running progress and cancellation with safe fixtures first. Do
not confirm the manual publication input or approve its protected environment
until the exact candidate passes the local terminal, SSH, fallback and
restoration matrix.
