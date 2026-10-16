# potter documentation

Start with the [project README](../README.md) for what potter does, its status and a quick start.

## Reference

| Document | Contents |
|---|---|
| [plan-format.md](plan-format.md) | The `potter.plan/v1` plan: canonical form and hashing, shape, `potter.toml` keys, cache keys, secrets in keys, remote cache entries, `--progress json` events, image annotations, attestations |
| [layer-budget.md](layer-budget.md) | Why layers are a resource in a microVM, the device ceiling per VMM, the composite policy, measured costs and how to choose a budget |
| [schema/potter.plan.v1.json](schema/potter.plan.v1.json) | JSON Schema of the plan, generated from the Rust types and checked in CI |

## Design and project history

| Document | Contents |
|---|---|
| [superpowers/specs/2026-10-04-potter-design.md](superpowers/specs/2026-10-04-potter-design.md) | The design spec: goals, threat model, components, plan IR, frontends, catalog, cache, executors, graph, supply chain. Section numbers (`spec §N`) in the code and CLI help refer to it |
| [superpowers/ROADMAP.md](superpowers/ROADMAP.md) | The roadmap across potter, kiln and vmkit, and the decisions shared by all three |
| [superpowers/HANDOFF.md](superpowers/HANDOFF.md) | Running notes per milestone: what landed, deferred findings, upstream follow-ups |

## Implementation plans

Each milestone was built from a written plan. They describe the work at the time; the code and the
reference documents above are authoritative where they differ.

| Milestone | Plan |
|---|---|
| P1a: plan IR, PHP/Laravel, Node, Bun, Deno and static detectors, CLI | [2026-10-05-potter-p1a-plan-core.md](superpowers/plans/2026-10-05-potter-p1a-plan-core.md) |
| P1b: Python, Ruby, Go and Rust detectors, monorepo targets | [2026-10-06-potter-p1b-detectors.md](superpowers/plans/2026-10-06-potter-p1b-detectors.md) |
| P2a: build engine and namespace executor | [2026-10-06-potter-p2a-engine.md](superpowers/plans/2026-10-06-potter-p2a-engine.md) |
| P2b: stopgap catalog for detected targets | [2026-10-07-potter-p2b-stopgap.md](superpowers/plans/2026-10-07-potter-p2b-stopgap.md) |
| P4a: VM executor (potter side) | [2026-10-11-potter-p4a-vm-executor.md](superpowers/plans/2026-10-11-potter-p4a-vm-executor.md) |
| P6a: remote cache, target graph, `--affected`, secrets | [2026-10-11-potter-p6a-cache-graph.md](superpowers/plans/2026-10-11-potter-p6a-cache-graph.md) |
| P6b: SBOM, provenance, signing, `potter verify` | [2026-10-11-potter-p6b-supply-chain.md](superpowers/plans/2026-10-11-potter-p6b-supply-chain.md) |
