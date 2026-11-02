# The runtime catalog

Detected Node, Bun, Deno, Python, Ruby, Go, Rust and static targets build on potter's own
runtime layers instead of upstream Docker Hub images. Each layer is built by potter from a recipe
under [`catalog/`](../catalog/README.md), listed in a signed index, resolved to digests at build
time and merged into one composite base. PHP and Laravel targets still build through the
stopgap (`crates/potter-exec/src/stopgap.rs`).

## Contents

One line per runtime, on x86_64 and aarch64:

| Layer | Line | Kind | |
|---|---|---|---|
| `os-min` | 13 (Debian trixie) | base | shared libraries, CA certificates, tzdata |
| `build-tools` | 13 | tool | compilers and -dev headers; build chains only |
| `node` | 26 | runtime | |
| `bun` | 1 | runtime | 1.4.4 |
| `deno` | 2 | runtime | 2.9.8 |
| `python` | 3.15 | runtime | |
| `uv`, `poetry`, `pipenv` | pinned in the recipe | tool | require `python` |
| `ruby` | 4.0 | runtime | with bundler |
| `go` | 1.27 | runtime | |
| `rust` | 1.99 | runtime | |
| `static-server` | 1 | tool | busybox httpd |

Not in the catalog: PHP and everything only PHP uses (`php`, `php-fpm`, `php-ext-*`, `composer`,
`nginx`, swoole, RoadRunner, FrankenPHP). A base that names any of those goes through the
stopgap whole; a ref that is neither a catalog layer nor PHP-only is a plan error (exit 2).

The detector's version table (`crates/potter-detect/src/catalog.rs`) carries the same lines. A
constraint none of them satisfies (`engines.node: "22"`, `.python-version` 3.12) fails at plan
time with exit 2, naming the constraint, where it came from and what the catalog has. There is
no fallback to the stopgap for non-PHP targets. The test `crates/potter/tests/catalog_agreement.rs`
fails when the table and the recipes disagree in either direction.

## The index

The index is an OCI artifact: an image manifest with `artifactType`
`application/vnd.potter.catalog.index.v1+json`, an empty config, and one layer, the index JSON
(same media type). The manifest's annotations carry the signature:

- `dev.potter.catalog.signature`: base64 ed25519 signature over the 32-byte SHA-256 of the index
  blob;
- `dev.potter.catalog.key`: the signing key's id, the first 16 hex digits of the SHA-256 of the
  raw public key.

The index JSON is canonical (JCS), at most 8 MiB and 10,000 entries:

```json
{
  "schema": "potter.catalog/v1",
  "serial": 12,
  "created": "2026-11-02T00:00:00Z",
  "entries": [
    {
      "name": "python", "line": "3.15", "version": "3.15.2", "arch": "x86_64", "kind": "runtime",
      "digest": "sha256:...", "diff_id": "sha256:...", "size": 41234567,
      "media_type": "application/vnd.oci.image.layer.v1.tar+zstd",
      "abi": {"os": "debian-13", "libc": "glibc-2.41", "python-abi": "cp315"},
      "requires": [{"name": "os-min"}], "conflicts": [], "env": {},
      "provides_sonames": ["libpython3.15.so.1.0"],
      "executor": "ns", "source": "https://www.python.org/ftp/python/3.15.2/", "license": "PSF-2.0",
      "supersedes": null, "fixes": []
    }
  ]
}
```

- `line` is what detectors and constraints name (`python@3.15`); `version` is the exact upstream
  release, for the SBOM, `catalog ls` and rebase.
- A rebuilt layer gets a new entry whose `supersedes` names the digest it replaces, for the same
  name, line and arch. Superseded entries stay listed and resolvable by digest; resolution picks,
  within a line, the entry nothing supersedes.
- `fixes` lists advisory ids the superseding entry fixes (see [Advisories](#advisories-fixes)).
- `env` is applied bottom first over the composite's config: `PATH` entries are prepended, other
  variables override.
- `executor` is `ns` for every entry this round. Anything built on an ns-built layer has `ns`
  provenance.
- Layers are ordinary OCI layer blobs (deterministic tar, zstd). kiln converts them to erofs when
  an image is converted. Each layer also holds its own metadata at
  `/usr/share/potter-catalog/<name>.json`, so a change to it changes the digest.

In a registry or an OCI layout the index is tagged `index` (the current one) and
`index-<hex>` (by its blob digest, for pinning), and each layer `layer-<hex>`.

## Verification

Every read of the catalog (`potter build`, `potter rebase`, `potter catalog ...`) verifies the
index before using it, and fails closed:

1. Fetch the manifest tagged `index` (or `index-<hex>` for a pinned index), with a size cap.
   Check its `artifactType`, its single layer's media type and size, and that it is signed.
2. Fetch the index blob by digest and check its SHA-256.
3. Check the signature against a trusted key, before parsing the index.
4. Parse the index, refusing unknown fields, bad digests and duplicates.
5. Refuse an index whose `serial` is lower than the highest serial already verified for this
   location (kept in `$POTTER_HOME/catalog/<location id>/serial`) unless it is pinned with
   `--catalog-index sha256:<hex>`. A pinned index must still verify.
6. Fetch each layer a build resolves to by the digest the index names, and hash it again on the
   way in.

A verification failure exits 3 and an unreachable catalog exits 4, both before any sandbox
starts. Detection never reads the catalog; only build-time resolution does.

## Locations and overrides

The default catalog is `ghcr.io/alfonsocampodonico/potter-catalog`, tag `index`. Two environment
variables change that, and each prints a `note:` line on stderr whenever it is used:

| Variable | Meaning |
|---|---|
| `POTTER_CATALOG` | Another catalog: a registry reference (tag `index` when it names none) or `oci-layout:DIR` |
| `POTTER_CATALOG_KEY` | Path to an ed25519 public key (PEM) trusted for this process, next to the compiled-in ones |

The trusted keys are compiled into the binary from `crates/potter-catalog/keys/*.pub`; the build
fails if one of them is not an ed25519 public-key PEM.

## Commands

| Command | Does |
|---|---|
| `potter catalog ls [--arch A] [--all] [--json]` | Lists the current entries for an architecture; `--all` lists every arch and the superseded entries |
| `potter catalog resolve <constraint>... [--arch A] [--json]` | Prints the layers, bottom first, and the composite key that constraints like `os-min python@3.15 uv` resolve to |
| `potter catalog verify [--catalog-index D]` | Verifies the index and fetches and re-hashes every current layer; exit 0 or 3 |
| `potter catalog affected <advisory-id \| sha256:digest> [--json]` | Lists the entries that fix an advisory, or the entries a digest supersedes and is superseded by |
| `potter key generate --ed25519 DIR` | Writes `DIR/catalog.key` (PKCS#8, 0600) and `DIR/catalog.pub` |

`--catalog-index D` on `build` and `rebase` pins the index.

## Key rotation

1. Generate the new key, commit its `.pub` under `crates/potter-catalog/keys/` next to the old
   one, and release potter (release N). Both keys are trusted.
2. Replace the `POTTER_CATALOG_KEY` secret with the new private key. The next publish is signed
   with it, and release N verifies it.
3. In release N+1, remove the old `.pub`. Releases before N do not trust the new key, so they
   stop verifying the catalog once it is signed with it: users upgrade to N or later.

A compromised key follows the same steps without waiting a release, and republishes the catalog
(a manual run) as soon as the new key is in.

## Bumping a pin

Pins move only by hand, in a pull request: there are no feed watchers and no scheduled rebuilds.
[`catalog/README.md`](../catalog/README.md) has the per-upstream details. In short:

1. Change `version` and the per-arch `sha256` in the recipe's `potter.toml`.
2. Change `version` (and `line` for a new line, with the detector's table) and `source` in its
   `layer.toml`. The smoke tests check versions, so a mismatch fails the build.
3. Open the pull request. CI builds every layer on both architectures, runs the overlap check and
   the smoke tests, and builds the end-to-end projects on the result. Nothing is published.
4. Merge. The push to `main` builds on the published catalog: an unchanged layer keeps its entry,
   a changed one gets a new entry superseding the old, and the index is published at serial + 1.

A new line (python 3.16, say) replaces the old one in this round's one-line catalog: change the
detector's table in the same pull request, or the agreement test fails.

## Advisories (`fixes`)

When a bump fixes advisories, list them in the layer's `layer.toml`:

```toml
fixes = ["CVE-2026-12345", "DSA-6001-1"]
```

The publish copies them onto the entry that supersedes the old one; `potter catalog affected
CVE-2026-12345` lists it. An image whose `dev.potter.catalog.layers` annotation names the
superseded digest is affected, and `potter rebase` moves it onto the new entry (see
[rebase.md](rebase.md#catalog-images)). Clear the list on the next bump.

## Maintainers: building and publishing

Two hidden subcommands do the catalog's CI. Both need the recipes' repository checkout; building
needs Linux and the sandbox set up as for `potter build --executor ns`.

```sh
potter catalog build CATALOG_DIR --out LAYOUT --sign-key KEY [--previous REF]
potter catalog build --merge LAYOUT --merge-key PUB ... --out LAYOUT --sign-key KEY [--previous REF]
potter catalog publish LAYOUT REGISTRY/REPOSITORY:TAG
```

- **`build`** builds every recipe under `CATALOG_DIR` in dependency order with the namespace
  executor (tenant `catalog`), for this machine's architecture. Each layer is what its recipe
  added over the layers it builds on; a recipe that deletes a file of those layers is refused.
  It then runs the overlap check (no regular file in two co-installable layers) and every smoke
  test on a composite of the layer's closure, and writes an OCI layout signed with `KEY`. With
  `--previous` (a registry reference or `oci-layout:DIR`, verified first) unchanged layers keep
  their entries, changed ones supersede, the previous entries stay, and the serial goes up by
  one; without it the index is serial 1. It prints one line per layer (`new`, `kept` or
  `supersedes <digest>`) and the serial. Exit 1 for an overlap or a failed smoke test, 2 for a
  recipe or usage error, 3 when `--previous` does not verify, 4 for infrastructure.
- **`build --merge`** builds nothing. It takes the layouts of one run's per-architecture builds,
  each made with the same `--previous`, verifies each with the `--merge-key` public keys they
  were signed with, and writes one index: the previous entries, then each layout's new ones, one
  serial above the previous catalog, signed once with `KEY`, holding every current layer. Layouts
  built on another catalog, or two layouts adding entries for the same architecture, are refused
  (exit 2). It needs no sandbox.
- **`publish`** verifies the layout with the trusted keys (the compiled ones and
  `POTTER_CATALOG_KEY`), checks it holds every current layer, then pushes the layers and the
  index. Registry credentials come from the Docker config (`DOCKER_CONFIG` or `~/.docker`).

Locally, with a throwaway key:

```sh
potter key generate --ed25519 /tmp/catalog-key
potter catalog build catalog --out /tmp/cat --sign-key /tmp/catalog-key/catalog.key
POTTER_CATALOG=oci-layout:/tmp/cat POTTER_CATALOG_KEY=/tmp/catalog-key/catalog.pub potter catalog ls
POTTER_TEST_CATALOG=1 POTTER_CATALOG=oci-layout:/tmp/cat \
  POTTER_CATALOG_KEY=/tmp/catalog-key/catalog.pub cargo test -p potter --test catalog_e2e
```

### CI

`.github/workflows/catalog.yml` runs on `ubuntu-24.04` (x86_64) and `ubuntu-24.04-arm` (aarch64),
with the same sandbox setup as the main workflow's sandbox job.

- **Pull requests** touching `catalog/**` or `crates/potter-catalog/**`: each architecture builds
  every layer into a layout signed with a key made for the job, which covers the overlap check
  and the smoke tests, then runs the agreement test and the end-to-end tests
  (`POTTER_TEST_CATALOG=1`) on that layout. Nothing is published.
- **Pushes to `main`** touching `catalog/**`, and **manual runs**: each architecture builds with
  `--previous ghcr.io/alfonsocampodonico/potter-catalog:index` and a throwaway key, and hands its
  layout and public key to the publish job. That job alone holds the signing secret and
  `packages: write`: it merges the two layouts with `build --merge` (one serial bump), signs the
  result with the `POTTER_CATALOG_KEY` secret, and runs `potter catalog publish` with the job's
  `GITHUB_TOKEN`. One publish runs at a time. A manual run with `first` set builds without
  `--previous`, for the very first publish.

## Setup

Done once by the repository owner.

1. Make the signing key, on a trusted machine:

   ```sh
   potter key generate --ed25519 ./catalog-key
   ```

2. Add the private key as the Actions secret `POTTER_CATALOG_KEY`, then delete the local copy:

   ```sh
   gh secret set POTTER_CATALOG_KEY < ./catalog-key/catalog.key
   ```

3. Commit the public key as `crates/potter-catalog/keys/catalog.pub`. potter builds from then on
   trust it.
4. Publish the first catalog: run the `catalog` workflow manually with `first` set.
5. Make the `potter-catalog` package on ghcr.io public, so builds (and the next publish's
   `--previous`) pull it anonymously.
