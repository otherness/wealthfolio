# Self-Hosting Wealthfolio

Wealthfolio ships an official Docker image so you can run the web edition on
your own hardware. Full self-hosting guides live on the website:

📘
**[wealthfolio.app/docs/guide/self-hosting](https://wealthfolio.app/docs/guide/self-hosting)**

This directory only holds short pointers per platform — the per-platform
artifacts (such as the Unraid Community Apps template) live in their own
repositories.

## Image

Multi-arch (`linux/amd64`, `linux/arm64`), published on every `v*.*.*` tag:

| Registry   | Image                                         |
| ---------- | --------------------------------------------- |
| Docker Hub | `wealthfolio/wealthfolio:latest` _(primary)_  |
| Docker Hub | `afadil/wealthfolio:latest` _(legacy mirror)_ |
| GHCR       | `ghcr.io/wealthfolio/wealthfolio:latest`      |

```bash
docker pull wealthfolio/wealthfolio:latest
```

Existing deployments that pin `afadil/wealthfolio:latest` keep working — both
Docker Hub repos receive the same multi-arch build from CI. New deployments
should prefer `wealthfolio/wealthfolio`.

## Permissions

The container runs as a non-root user (UID/GID **1000:1000**).

**Fresh install:** Docker named volumes work out of the box. For a bind mount,
make the host directory writable by UID 1000:

```bash
mkdir -p ./data && sudo chown -R 1000:1000 ./data
```

**Upgrading from an older image:** existing data is owned by `root` and must be
chowned once. Pick the line that matches your setup:

```bash
# named volume
docker run --rm -v <your-volume>:/data alpine chown -R 1000:1000 /data
# bind mount
sudo chown -R 1000:1000 /path/to/your/data
```

## Database encryption (optional)

Encryption at rest is **off by default**; existing databases keep working
untouched. When enabled, the whole database file is encrypted with SQLCipher
using a key derived from `WF_SECRET_KEY` — nothing extra is stored, so a
database file copied to any instance sharing that secret simply opens.

Two separate things control it, and they do different jobs:

- **`wealthfolio-server db encrypt`** converts an existing database. It runs
  offline, with the server stopped, because replacing the database file requires
  that nothing is connected to it.
- **`WF_DB_REQUIRE_ENCRYPTION=1`** states a requirement and never converts
  anything. Against an existing database it asserts the file is encrypted and
  refuses to start if it is not. For a database that does not exist yet, it
  decides how that database gets created.

So what you need depends on whether you already have data.

### New installation (no database yet)

There is nothing to convert, so there is no command to run — set the variable
before the first start and the database is created encrypted from the first
write. With Compose:

```yaml
services:
  wealthfolio:
    image: wealthfolio/wealthfolio:latest
    environment:
      WF_SECRET_KEY: "<your-secret>"
      WF_DB_REQUIRE_ENCRYPTION: "1"
    volumes:
      - wealthfolio-data:/data
    ports:
      - "8088:8088"
```

With plain Docker, the same two variables:

```bash
docker run -d --name wealthfolio -p 8088:8088 -v wealthfolio-data:/data -e WF_SECRET_KEY='<your-secret>' -e WF_DB_REQUIRE_ENCRYPTION=1 wealthfolio/wealthfolio:latest
```

Confirm it in **Settings → General → Database Encryption**, which reports the
state of the file itself rather than the value of the variable.

### Existing installation (converting your data)

**1. Stop the server.** The conversion replaces the database file, so it aborts
untouched if anything is still connected — running it with `docker exec` against
a live container will not work.

```bash
docker compose stop wealthfolio
```

Plain Docker: `docker stop wealthfolio`.

**2. Back up the data directory.** Copy the whole directory rather than just the
`.db` file: if a `-wal` file sits beside it, the newest transactions live there
and copying the database alone loses them.

```bash
docker run --rm -v wealthfolio-data:/data -v "$PWD":/backup alpine tar czf /backup/wealthfolio-backup.tar.gz -C /data .
```

This archive is your rollback point. `db encrypt` does take its own
pre-operation backup, but deletes it on success — that copy is an unencrypted
duplicate of everything you just encrypted, so it goes to a private scratch
directory and is removed as soon as the encrypted database verifies (and cleared
at the next start if a crash interrupts). Keep your own archive until the server
is back up and the data looks right.

**3. Convert the database.** With Compose:

```bash
docker compose run --rm wealthfolio wealthfolio-server db encrypt
```

With plain Docker, run a one-shot container over the same volume and the same
`WF_SECRET_KEY` — the key is derived from it, so a different secret produces a
database the server cannot open:

```bash
docker run --rm -v wealthfolio-data:/data -e WF_SECRET_KEY='<your-secret>' wealthfolio/wealthfolio:latest wealthfolio-server db encrypt
```

Expect `Database at /data/wealthfolio.db is now encrypted`. The command refuses
to run when there is no database at `WF_DB_PATH` instead of creating an empty
one, so a mistyped path or an unmounted volume is a clear error rather than a
silently empty database. Allow roughly twice the database size in free space on
the volume while it runs.

**4. Start again with the requirement set.** Add `WF_DB_REQUIRE_ENCRYPTION=1` to
the environment, then bring the server up:

```bash
docker compose up -d wealthfolio
```

With plain Docker a variable cannot be added to an existing container: remove it
with `docker rm wealthfolio` and re-run your original `docker run` line with
`-e WF_DB_REQUIRE_ENCRYPTION=1` appended. Calling `docker start` on the old
container instead leaves the variable unset, and the server will refuse to boot
against the now-encrypted database.

### Platform notes

**Unraid.** The Community Apps template runs the container as `--user=99:100`
(`nobody:users`) rather than the image default of `1000:1000`. The one-shot
conversion container must use the **same** user and the same appdata path, or
the files it creates — the converted database and the private `scratch/`
directory, which is created owner-only — end up owned by a user the server
cannot read or write:

```bash
docker run --rm --user=99:100 -v /mnt/user/appdata/wealthfolio:/data -e WF_SECRET_KEY='<your-secret>' wealthfolio/wealthfolio:latest wealthfolio-server db encrypt
```

Add `WF_DB_REQUIRE_ENCRYPTION` as a Variable in **Docker → wealthfolio → Edit**
afterwards. Note that Unraid's Console button execs into a _running_ container,
which is exactly what the conversion refuses to work against — run the command
above from the Unraid terminal instead, with the container stopped.

Appdata backups (CA Backup and friends) copy the encrypted database as-is, so it
is only restorable somewhere that has the same `WF_SECRET_KEY`. Back that secret
up separately from the appdata archive, or the archive is unusable.

**Installs without Docker** (for example the Proxmox LXC from
community-scripts): the same two steps apply, minus the container. Stop the
service, run `wealthfolio-server db encrypt` directly as the same user the
service runs as, add `WF_DB_REQUIRE_ENCRYPTION=1` to the unit's environment (or
its `EnvironmentFile`), and start it again.

Builds from source now link SQLCipher against a vendored OpenSSL, which needs
`perl` and a C toolchain present at build time — worth knowing if you maintain
your own build script, since a missing `perl` fails the build rather than
degrading gracefully.

### Turning it off

Reverse the same way: stop the server, run `wealthfolio-server db decrypt` in
place of `db encrypt`, and remove `WF_DB_REQUIRE_ENCRYPTION` from the
environment. Unlike `db encrypt`, `db decrypt` keeps its pre-operation backup in
`<data>/backups/`.

### If the server refuses to start

**It fails closed in both directions.** The server refuses to start if the
database is encrypted while `WF_DB_REQUIRE_ENCRYPTION` is unset, and if the
variable is set while the database is still plaintext. Neither state is silently
accepted: one would run a configuration you did not ask for, the other would
claim encryption the file does not have. In practice this error means you
completed one half of the change and not the other, and the message names the
command that finishes it.

Backups taken through the app inherit the database's encryption, so on an
encrypted server they are encrypted too — and open on any instance sharing
`WF_SECRET_KEY`.

### Rotating `WF_SECRET_KEY`

`WF_SECRET_KEY` derives three independent keys: the session/JWT signing key, the
`secrets.json` encryption key, and the database key. **Rotate all three
together**, or the server will not start:

1. Stop the server.
2. With the _old_ `WF_SECRET_KEY`, run `wealthfolio-server db decrypt` and unset
   `WF_DB_REQUIRE_ENCRYPTION`.
3. Set the new `WF_SECRET_KEY`. `secrets.json` migrates on the next boot; all
   existing sessions are invalidated and users sign in again.
4. Run `wealthfolio-server db encrypt` with the new secret and set
   `WF_DB_REQUIRE_ENCRYPTION=1`.

Backups taken before the rotation stay readable only with the old secret. Keep
it until you no longer need them.

## Platform pointers

- [**Docker / Docker Compose**](https://wealthfolio.app/docs/guide/self-hosting):
  the canonical path. Full walkthrough on the website.
- [**Unraid**](./unraid/): install via Community Apps. The CA template is
  maintained at
  [`wealthfolio/wealthfolio-unraid`](https://github.com/wealthfolio/wealthfolio-unraid).
- [**Proxmox VE**](./proxmox/): LXC via community-scripts, Docker-in-LXC, or
  Docker VM.
