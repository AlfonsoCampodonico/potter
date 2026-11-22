#!/usr/bin/env bash
# Builds what potter's VM tests need and runs them (Linux with KVM).
# Usage: scripts/vm-tests.sh <vmkit kernel> [cargo test args...]
# Needs the VMMs on PATH, a static /usr/bin/busybox (busybox-static), uidmap and a subuid range,
# passt and nftables, the musl target for this arch, and vmkit-sandbox's AppArmor profile at
# $CARGO_TARGET_DIR/debug/vmkit-sandbox (the script prints the install command if it is missing).
set -euo pipefail
# Exactly one kernel: an unquoted glob that matched several would hand the rest to cargo.
if [ $# -lt 1 ] || [ ! -f "$1" ]; then
    echo "vm-tests: the first argument must be the kernel file (got ${1:-nothing})" >&2
    exit 2
fi
kernel=$(realpath "$1"); shift
for arg in "$@"; do
    [ "$arg" = -- ] && break
    if [ -f "$arg" ]; then
        echo "vm-tests: more than one kernel ($kernel, $arg ...): name exactly one" >&2
        exit 2
    fi
done
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
target=$(realpath -m "${CARGO_TARGET_DIR:-$root/target}")
musl=$(uname -m)-unknown-linux-musl
# One `cargo metadata`; each lookup is its own assignment, so a failure stops the script.
metadata=$(cargo metadata --locked --format-version 1)
manifest() { python3 -c "import json,sys; print(next(p['manifest_path'] for p in json.load(sys.stdin)['packages'] if p['name'] == sys.argv[1]))" "$1" <<<"$metadata"; }
kiln_run_manifest=$(manifest kiln-run)
kiln_ws=$(dirname "$(dirname "$(dirname "$kiln_run_manifest")")")
vmkit_manifest=$(manifest vmkit)
cargo build --locked --release --manifest-path "$kiln_ws/Cargo.toml" --target "$musl" -p kiln-init --target-dir "$target/kiln"
cargo build --locked --manifest-path "$vmkit_manifest" --bin vmkit-sandbox --target-dir "$target"
# vmkit's install-apparmor.sh names the profile after the helper's path. A glob profile can cover
# the path under another name, so a missing file is only a hint.
helper=$target/debug/vmkit-sandbox
profile=/etc/apparmor.d/vmkit-sandbox-$(printf %s "$helper" | sha256sum | cut -c1-12)
if [ "$(sysctl -n kernel.apparmor_restrict_unprivileged_userns 2>/dev/null || echo 0)" = 1 ] && [ ! -e "$profile" ]; then
    echo "vm-tests: no $profile; unless another profile covers $helper, install one once:" \
        "$(dirname "$vmkit_manifest")/scripts/install-apparmor.sh $helper" >&2
fi
export VMKIT_SANDBOX=$helper
export POTTER_TEST_KERNEL=$kernel
export POTTER_TEST_KILN_INIT=$target/kiln/$musl/release/kiln-init
export POTTER_REQUIRE_VM_TESTS=1 POTTER_REQUIRE_SANDBOX_TESTS=1
# Two runs: `--test vm` would select potter-exec's (absent) `vm` test target too, and skip its tests.
cargo test --locked -p potter --test vm "$@"
cargo test --locked -p potter-exec "$@"
