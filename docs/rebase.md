# Rebase

`potter rebase` swaps a built image's run base for a patched one without rebuilding the app, as
Cloud Native Buildpacks' `rebase` does (design spec §16). A security fix in `debian:trixie-slim` or
`php:8.4-cli` reaches the image in seconds: no step runs again, and the image's own layers are kept
byte for byte unless they depend on the base.

```sh
potter build . --executor ns
# ... the base's tag is rebuilt upstream with a fix ...
potter rebase potter/app:latest --executor ns
```

## What the run base is

The run base is the bottom of the image that a rebase replaces:
- **With `export`:** the image's run base and its `runtime_steps`. The export layer and the files
  layer sit above it.
- **Without `export`:** the OCI base the build chain started from (after catalog lowering). Every
  build step is above it.

At build time potter converts the run base into a kiln image of its own (untagged) and converts the
image with that base recorded (kiln's `BaseRecord`, the same record `kiln convert --base` writes). The
image's source manifest also carries:

| Annotation | Value |
|---|---|
| `dev.potter.run_chain` | The run base and runtime steps, as canonical JSON: what a rebase re-runs |
| `dev.potter.max_layers` | The layer budget it was built with, which a rebase keeps |

The image inherits its run base's OCI labels, so a rebase can tell inherited labels from the image's
own. The layer budget never merges across the run base's top (see
[layer-budget.md](layer-budget.md#the-composite-policy)), so every image with layers above its run
base can be rebased.

## What `potter rebase` does

1. Reads the image's base record and annotations. An image built before this feature records
   neither and is refused (exit 2): rebuild it once.
2. Resolves the run base again:
   - an OCI base by its recorded reference, never its pinned digest, so a rebuilt tag is picked up;
   - with `--base REF`, another OCI reference instead (not for catalog bases, exit 2);
   - a catalog base against the catalog, from the constraints the image recorded, behind the
     catalog gates (see [Catalog images](#catalog-images));
   - a non-PHP catalog base built before P3 (no `dev.potter.catalog.*` annotations), or a PHP
     one, by lowering it through the stopgap again (`stopgap.rs`). The catalog isn't loaded for
     these; moving such an image onto the catalog takes a rebuild.
3. Re-runs the runtime steps on it with the namespace executor. They're `apt` steps with no inputs,
   cached like any step, so this is usually a cache hit or a short install.
4. Converts the new run base under the image's layer budget. If it's the run base the image already
   has, nothing is written: `already on ...`.
5. Calls kiln's rebase (`kiln_image::rebase`), which:
   - keeps every app layer that doesn't inherit from the base, byte for byte;
   - re-derives the others from their stored source tars on the new base;
   - lets inherited env vars and labels follow the new base;
   - refuses, with the reason, when the base changes in a way the app could notice (below).
6. Tags the result: `--tag NAME`, or the image's own name when it was given by name.

A refused rebase tags nothing and names every reason. The image needs a rebuild. kiln's refusals
exit 1, the catalog gates' exit 2.

## Catalog images

An image on a catalog run base records its catalog layers, the constraints they resolved from
(`name@line`), the index, the layers above its run base that a rebase keeps, and the sonames its
ELF files need (see [plan-format.md](plan-format.md#image-annotations-spec-51-18)). A rebase:

1. Resolves the recorded constraints again, against the catalog's current index or the one
   `--catalog-index` pins. A layer some newer entry supersedes is replaced by it.
2. Runs the catalog gates. Any refusal stops the rebase before any step runs, and it exits 2:
   - `line`: a layer changes its line, leaves the run base or joins it, or the index no longer
     lists a layer the image is on;
   - `abi`: a layer changes any ABI value (the refusal names the key, the old and the new value);
   - `sonames`: the image records no `dev.potter.sonames.needed`, or the app needs a soname that
     no new catalog layer's `provides_sonames` lists and no layer above the run base provides.
     Shared objects the app ships itself never cause a refusal.
3. Builds the new composite and re-runs the runtime steps on it, then goes on through kiln's
   rebase as for any image. kiln's gates stay in force. An image with `export` keeps its export
   and files layers byte for byte. An image without one takes kiln's re-derive path.

The run base records the catalog layers it was built from on its own manifest. A rebased image
keeps the source manifest it was built from, so this record is how a later rebase knows which
layers the image is on now.

The result's `replaced` lists the `[old, new]` catalog entry digests that were swapped. The
composite's display name (`potter-catalog@<index>`) is never reported as the replaced base.

`--dry-run` resolves and runs the catalog gates, prints the replacements and every gate's verdict,
and writes nothing: no step runs, no composite is built and nothing is tagged. Loading the catalog
still keeps its verified index, as any read of it does. kiln's gates need the new run base built,
so a dry run reports them as not checked. It exits 2 when a catalog gate refuses, and it takes
catalog images only.

## Refusals

kiln's gates decide (kiln `docs/format.md`, kiln spec §16.3). A rebase is refused when:
- the platform changes;
- `/etc/os-release` changes `ID` or its release (`VERSION_ID`, for example Debian 13 to 14);
- a `<lang>.version` label changes its major.minor;
- the new base drops an env var the old one set;
- the base's start contract (entrypoint, cmd, user, working directory) changes;
- any file the app's layers shadow (copy up, replace or delete) differs between the two bases. This
  catches a `sed -i` over a base config file.

## Options

| Option | Meaning |
|---|---|
| `--executor ns` | Required, as for `potter build`: the runtime steps run in Linux namespaces |
| `--base REF` | Rebase onto another OCI image instead of resolving the recorded reference again |
| `--tag NAME` | Tag the result (default: `IMAGE`, when it's a name; a digest needs `--tag` to tag anything) |
| `--tenant ID` | Whose cache the runtime steps use (default `local`) |
| `--catalog-index D` | Resolve a catalog run base against this index (`sha256:<hex>`, still verified) instead of the current one |
| `--dry-run` | Catalog images: print what would be replaced and every gate's verdict, writing nothing (`--executor` not needed) |
| `--json` | Print `digest`, `tag`, `unchanged`, `old_base`, `new_base`, `base_ref`, `base_digest`, `reused`, `changed`, `executed`, `cached`, `replaced`. With `--dry-run`: `dry_run`, `digest`, `index`, `replaced`, and `gates` (`gate`, `verdict`, `reasons`) |

## Not done yet

- **Pushing a rebased image.** `potter build --output registry=` pushes the OCI image potter
  assembled, with its SBOM and provenance. A rebase writes a kiln image to the local store only. So
  there is no provenance record naming the replaced base yet (spec §17, P12). That needs the rebased
  OCI image assembled and pushed with its own attestations.
- **The `DT_NEEDED` soname check for OCI bases.** Catalog images get it. For an OCI base, kiln's
  gates catch release and shadowed-file changes, but not a native library in the app whose soname
  the new base no longer provides.
- **Multi-platform images.** potter builds one platform per image, and rebase takes those.
