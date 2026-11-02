# The catalog's recipes

One directory per catalog layer. `layer.toml` says what the layer is (name, line, version, kind,
requires, ABI values beyond `os`/`libc`, env, source, license, the smoke test). `potter.toml` is
the explicit plan that builds it. `potter catalog build` (hidden) builds them in dependency order
with the namespace executor, checks that no two co-installable layers hold the same file, runs
every smoke test, and writes a signed OCI layout.

| Layer | Builds on | What it installs |
|---|---|---|
| `os-min` | `debian:trixie-slim` (index digest) | shared libraries, CA certificates, tzdata from snapshot.debian.org |
| `build-tools` | os-min | compilers and -dev headers (apt, same snapshot) |
| `node`, `bun`, `deno`, `go`, `rust`, `uv` | os-min (bun, deno: + build-tools to unzip) | upstream Linux builds |
| `python`, `ruby` | os-min + build-tools | compiled from source into `/usr/local` |
| `poetry`, `pipenv` | os-min + python | a venv in `/usr/local/lib/<tool>` from hashed wheels |
| `static-server` | os-min | busybox httpd from Debian's busybox-static, unpacked |

Only os-min and build-tools hold Debian packages. Every other layer installs under `/usr/local`
and leaves no dpkg state, and no recipe may delete a file of the layers it builds on.

## Bumping a pin

Every download is pinned by URL and SHA-256, one per architecture, picked with
`case "$(uname -m)"`. To bump one:

1. Change `version` (and the per-arch `sha256`) in `potter.toml`. Take checksums from upstream:
   nodejs.org `SHASUMS256.txt`, the GitHub release's asset digests (bun, deno, uv), the go.dev
   JSON feed, python.org's release files, cache.ruby-lang.org's `index.txt`,
   static.rust-lang.org (`channel-rust-<v>.toml.sha256`, `rustup/archive/<v>/<triple>/rustup-init.sha256`).
2. Change `version` (and `line` for a new line) and `source` in `layer.toml`. The smoke tests
   check the version, so a mismatch fails the build.
3. poetry and pipenv: change `requirements.in` and regenerate `requirements.txt` with the command
   in their `potter.toml`.
4. Debian: the `debian:trixie-slim` digest and the snapshot date are in `os-min/potter.toml`; the
   same date is in `build-tools/potter.toml` and `static-server/potter.toml`. Bump them together,
   and `version` of os-min and build-tools to the point release (`/etc/debian_version`).

A changed layer gets a new digest and supersedes its current entry; unchanged layers keep theirs.
Changing os-min changes every layer's ABI metadata, so everything supersedes.

## Advisory ids (`fixes`)

When a bump fixes advisories, list them in that layer's `layer.toml`:

```toml
fixes = ["CVE-2026-12345", "DSA-6001-1"]
```

The publish copies them onto the entry that supersedes the old one, and
`potter catalog affected <id>` finds them. Clear the list on the next bump.

## Building locally

On Linux, with the sandbox set up as for potter's other namespace builds:

```sh
potter key generate --ed25519 /tmp/catalog-key
potter catalog build catalog --out /tmp/cat --sign-key /tmp/catalog-key/catalog.key
POTTER_CATALOG=oci-layout:/tmp/cat POTTER_CATALOG_KEY=/tmp/catalog-key/catalog.pub potter catalog ls
```

`--previous <registry ref | oci-layout:dir>` builds on a published catalog: unchanged layers keep
their entries and the serial goes up by one. The end-to-end tests build projects on such a
layout:

```sh
POTTER_TEST_CATALOG=1 POTTER_CATALOG=oci-layout:/tmp/cat \
  POTTER_CATALOG_KEY=/tmp/catalog-key/catalog.pub cargo test -p potter --test catalog_e2e
```
