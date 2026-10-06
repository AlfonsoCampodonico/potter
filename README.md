# potter

Build [kiln](../kiln) microVM images from source. potter detects PHP/Laravel, Node, Bun, Deno and
static-site projects, or reads an explicit `potter.toml`, and produces a canonical build plan.
Later milestones execute plans in microVMs or rootless namespaces with input-precise caching.

    potter plan [PATH] [--target T] [--json | --explain]
    potter detect [PATH]

Detection only parses files; it never runs your code. See `docs/plan-format.md` and the design
spec in `docs/superpowers/specs/`.
