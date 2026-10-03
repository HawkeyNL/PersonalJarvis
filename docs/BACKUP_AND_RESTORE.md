# Home Node backup and restore

`jarvis-backup` writes one encrypted archive per run for disaster recovery.
The owner runs it as root, by hand or with the opt-in daily timer, and copies
archives off the machine by hand. Nothing is uploaded.

> **Only one archive stays on the host.** Each successful run replaces the
> previous archive. A disk failure, a compromised host or a backup of already
> damaged data leaves you with nothing older. Copy archives off the machine
> regularly (see [Copying off the machine](#copying-off-the-machine)).

## What is in an archive

`jarvis-backup-YYYY-MM-DD.tar` (mode 0600) and `jarvis-backup-YYYY-MM-DD.tar.sha256`.
The tar contains three gpg-encrypted members:

| Member | Content |
|---|---|
| `surrealdb.surql.zst.gpg` | Logical `surreal export` of the Core namespace/database (one consistent read transaction) |
| `etc-jarvis.tar.zst.gpg` | `/etc/jarvis` including `secrets/`, with owner/group names and modes |
| `manifest.json.gpg` | Format version, time, SurrealDB image digest, per-table counts (live before, live after, restored), export and member hashes |

Members are encrypted to the owner's **public** keys. The Home Node holds no
private key, so it cannot decrypt its own backups.

Not included, by design:
- Signing keys (updater, Android, Apple). Keep separate offline copies.
- Core release binaries. Reinstall verified releases instead of restoring binaries.
- `/var/lib/jarvis/agents` and worker state. Both are reconstructable.
- Data of other services on this machine.

## What a run checks

1. It counts rows per table in the live database, exports, and counts again.
2. It imports the export into a disposable, networkless SurrealDB container
   (same pinned image, in-memory, 1 GiB limit). It requires the same tables,
   and each restored count must lie between the two live counts. The live
   database is never written.
3. It encrypts, writes the archive atomically, verifies the checksum and
   member layout, and only then removes the previous archive. The archive it
   just published is never removed, even when another archive carries a later
   date (clock skew or a manual copy).

If any step fails before publishing, nothing is published, the previous
archive is untouched, and the temporary files are removed. A second run on the same day
replaces that day's archive and checksum, each with an atomic rename. The
plaintext export, capped at 1 GiB, exists only on tmpfs below `/run` during
the run. tmpfs can be swapped to disk.

The run holds the Core updater lock and waits up to 10 minutes for a running
update.

## One-time setup

### 1. Owner keys, on the Mac

```bash
brew install gnupg
gpg --quick-gen-key 'Jarvis backup <you@example.com>' default default never
gpg --quick-gen-key 'Jarvis backup recovery <you@example.com>' default default never
gpg --list-keys --with-colons | awk -F: '$1 == "fpr" { print $10 }'
gpg --armor --export <MAC_FPR> <RECOVERY_FPR> > jarvis-backup-recipients.asc
```

Use a strong passphrase for both keys. Move the recovery private key off the
Mac: export it (`gpg --armor --export-secret-keys <RECOVERY_FPR>`), store it
offline (paper or a hardware token), and delete it from the Mac.
**If every private key is lost, every archive is unreadable.**

### 2. Host configuration, on the Home Node

```bash
sudo install -d -o root -g root -m 0700 /etc/jarvis-backup /var/backups/jarvis-dr
sudo install -o root -g root -m 0600 jarvis-backup-recipients.asc /etc/jarvis-backup/recipients.asc
sudo install -o root -g root -m 0600 /dev/stdin /etc/jarvis-backup/backup.conf <<'EOF'
destination=/var/backups/jarvis-dr
recipients=<MAC_FPR>,<RECOVERY_FPR>
EOF
```

`recipients.asc` must contain exactly the configured public keys and no
private key. The run refuses anything else.

## Running a backup

`jarvis-backup` ships inside every verified Core release as
`/opt/jarvis/current/jarvis-backup`. `prepare-home-node.sh` links
`/usr/local/sbin/jarvis-backup` to it. On a Home Node that was prepared
before this, create the link once:

```bash
sudo ln -sfn /opt/jarvis/current/jarvis-backup /usr/local/sbin/jarvis-backup
```

```bash
sudo jarvis-backup create
sudo jarvis-backup verify /var/backups/jarvis-dr/jarvis-backup-YYYY-MM-DD.tar
```

After a rollback to a release older than the one that introduced it, the
command is absent until the next update; existing archives are unaffected.

Exit code 75 means another backup, a Core update or a configuration change
was running. Retry later.

## Daily schedule

Core releases ship `jarvis-backup.timer`, **disabled**. Installers and updates
never enable it. Finish the one-time setup first: without
`/etc/jarvis-backup/backup.conf` every run fails and points to this document.
The destination must not be below `/usr`, `/boot`, `/etc`, `/home` or `/root`; the
service sees those read-only.

Test one run, then enable the timer:

```bash
sudo systemctl start jarvis-backup.service
journalctl -u jarvis-backup --since today
sudo systemctl enable --now jarvis-backup.timer
systemctl list-timers jarvis-backup.timer
```

To stop scheduled backups:

```bash
sudo systemctl disable --now jarvis-backup.timer
```

The timer runs daily at 06:00 local time, plus a random delay of up to
15 minutes. A run missed while the machine was off runs at the next boot.
The schedule is fixed: a drop-in that changes `OnCalendar`, `Persistent` or
`RandomizedDelaySec` blocks Core updates until it is removed.

A run that finds another backup, a Core update or a configuration change in
progress exits with code 75. `systemctl` then shows the service as failed; the
previous archive is kept and the next run is the following morning.

**Disable the timer before rolling back** to a Core release without scheduled
backups. The updater refuses that rollback while the timer is enabled or
active, or a backup is running.

## Copying off the machine

```bash
# On the Home Node: hand one archive to your user account.
sudo install -o "$USER" -m 0600 /var/backups/jarvis-dr/jarvis-backup-YYYY-MM-DD.tar{,.sha256} ~/
# On the Mac:
scp 'homenode:jarvis-backup-YYYY-MM-DD.tar*' ~/JarvisBackups/
cd ~/JarvisBackups && shasum -a 256 -c jarvis-backup-YYYY-MM-DD.tar.sha256
# Sign the copy you verified. The public recipient keys let anyone build a
# valid-looking archive and checksum; only your signature proves this one is yours.
gpg --local-user <MAC_FPR> --detach-sign jarvis-backup-YYYY-MM-DD.tar
# Back on the Home Node:
rm ~/jarvis-backup-YYYY-MM-DD.tar ~/jarvis-backup-YYYY-MM-DD.tar.sha256
```

## Quarterly decrypt drill, on the Mac

This proves the full chain without the key ever touching the Home Node.

```bash
mkdir drill && tar -C drill -xf jarvis-backup-YYYY-MM-DD.tar
gpg -d drill/manifest.json.gpg | jq '.export_sha256, .tables'
gpg -d drill/surrealdb.surql.zst.gpg | zstd -d | shasum -a 256
```

The last hash must equal `export_sha256` in the manifest. Repeat the drill
once with the recovery key. Delete `drill/` afterwards.

## Disaster recovery onto a fresh Home Node

Restore the database and `/etc/jarvis` from the **same** archive: the Core
database password in `core.env` pairs with the `core` user inside the export.

1. Install Ubuntu, then a verified Core release with the normal setup.
   Stop before the SurrealDB initialisation step. Never restore binaries from
   a backup.
2. On the Mac, check your signature, then decrypt the two data members into a
   private directory:
   ```bash
   gpg --verify jarvis-backup-YYYY-MM-DD.tar.sig jarvis-backup-YYYY-MM-DD.tar
   umask 077 && mkdir restore
   tar -C restore -xf jarvis-backup-YYYY-MM-DD.tar
   gpg -d restore/etc-jarvis.tar.zst.gpg | zstd -d > restore/etc-jarvis.tar
   gpg -d restore/surrealdb.surql.zst.gpg | zstd -d > restore/export.surql
   ```
   Copy both to the new host into a 0700 directory, for example
   `/root/restore`, and delete the plaintext copies on the Mac. Plaintext on an
   SSD is not reliably erased by `rm`; prefer an encrypted volume on the Mac
   and a host with full-disk encryption.
3. Restore `/etc/jarvis`. Only regular files and directories under
   `etc/jarvis/` are accepted. Extract as root so owners map by name:
   ```bash
   sudo tar -tvf /root/restore/etc-jarvis.tar | awk '$1 !~ /^[-d]/ { bad = 1 } END { exit bad }' \
     && [ -z "$(sudo tar -tf /root/restore/etc-jarvis.tar | grep -v '^etc/jarvis\(/\|$\)')" ] \
     && sudo tar -C / -xpf /root/restore/etc-jarvis.tar \
     || echo 'STOP: unexpected entry or extraction error'
   ```
4. Start SurrealDB with the restored `surrealdb.env`
   (`sudo /usr/local/libexec/jarvis/start-production-surrealdb`). Keep Core
   stopped. Confirm that the database is empty: `INFO FOR DB` must list no
   tables, or report that the database does not exist yet.
   **Never import into a non-empty database.**
5. Import:
   The image has no shell, so place the export in the root-only data
   directory, which the container mounts as `/data`:
   ```bash
   sudo install -o root -g root -m 0600 /root/restore/export.surql /var/lib/jarvis/surrealdb/restore-export.surql
   sudo docker compose --env-file /etc/jarvis/surrealdb.env -f /opt/jarvis/surrealdb/docker-compose.yml \
       exec -T surrealdb /surreal import --endpoint http://127.0.0.1:8000 \
       --namespace jarvis --database core /data/restore-export.surql
   sudo rm /var/lib/jarvis/surrealdb/restore-export.surql
   ```
   Use the namespace and database from `/etc/jarvis/surrealdb-core-provisioned`.
6. Start Core and run `verify-home-node`. Check owners and modes under
   `/etc/jarvis`, the service identities and the listeners before enabling
   public ingress. Delete `/root/restore`.

### After a suspected compromise

Do not trust restored credentials. Before re-enabling ingress, rotate provider
API keys, the SurrealDB root and Core passwords, and SSH/deploy keys and
private repository tokens, and revoke every device and session. An older
database can also bring back devices and sessions that were revoked after
the backup was taken; revoke those again.
