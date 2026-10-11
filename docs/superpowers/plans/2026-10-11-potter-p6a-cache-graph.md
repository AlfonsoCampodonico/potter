# potter P6a: remote cache, graph, secrets, JSON progress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** the turborepo half of potter: a tenant-scoped remote cache, multi-target builds with `copy_from` between targets, `--affected`, build secrets, and `--progress json`.

**Spec:** `docs/superpowers/specs/2026-10-04-potter-design.md`:
- §5.4 (secrets);
- §8.1 (the key's secrets HMAC);
- §8.3 (remote cache);
- §9.6 step 2 (`CopyFrom`);
- §10 (graph, `--affected`);
- §11 (`--progress json`);
- §14.3 (remote cache scenario);
- §14.4 (P1 tenants, P5 secrets).

P6b, the next plan, is supply chain (SBOM, provenance, signing, `potter verify`) and build Jobs (§17, §18).

## Global Constraints
- P2a's constraints still hold.
- **Remote cache:**
  - Never required: pull and push failures are warnings (§12), and a build without `--cache-from`/`--cache-to` behaves exactly as before.
  - The repository is `<prefix>/<tenant>/cache`. Tags are the key's hex digits.
  - Manifests are OCI image manifests with `artifactType: application/vnd.potter.cache.v1+json` and a config blob `{class, potter, parent}`. They hold at most one layer: potter's uncompressed tar.
- **Secrets:**
  - Values never reach a layer, the plan, the index or the remote cache. Only their HMAC (keyed by the tenant key) enters keys.
  - Logs are redacted best-effort.

## Tasks
1. **Secrets:**
   - The key's secrets field: `HMAC-SHA256(tenant_key, id ‖ 0 ‖ value)`, sorted by id.
   - `--secret id=X,src=FILE`.
   - A read-only bind of the file at the target for that step only; the empty mount point is left out of the seal.
   - Redaction of values in streamed logs and failure tails.
2. **`--progress json`:** newline-delimited events on stdout: `step_start`, `cache_hit`, `sealed`, `log`, `failed`, `warning`, `pushed`, `target_built`. `plain` keeps today's stderr lines.
3. **Remote cache:**
   - `potter-cache::remote`: a `Remote { registry, prefix }` with `get(key)`, which fetches the entry and blob into the store and verifies them, and `put(key, entry, layer)`.
   - The chain driver checks the remote on a local miss (longest prefix), records hits locally, and pushes each sealed step in a background thread.
   - `--cache-from` / `--cache-to` / `POTTER_REMOTE_CACHE`.
4. **Graph:**
   - Targets build in dependency order (`copy_from` targets first; a cycle is a plan error).
   - `CopyFrom { from: Target }` is a host-built layer from the source target's final chain: Docker `COPY --from` semantics, `owner`/`mode` overrides. Its digest is the inputs digest.
   - `Stage` sources stay refused (P5).
5. **`--affected <git-ref>`:** plans the ref's tree (`git archive`) and HEAD's. A target is affected when its plan hash, or any inputs or copy digest, differs. Unaffected targets are reported `unchanged` and skipped.
