//! The layer budget: how many erofs layers (virtio-blk disks) an image may have (`docs/layer-budget.md`).

/// The budget when a plan sets none: kiln's own `--max-layers` default.
pub const DEFAULT_MAX_LAYERS: u32 = 10;

/// The smallest budget: the composite base, one layer above it and the top layer.
pub const MIN_MAX_LAYERS: u32 = 3;

/// The largest budget: kiln-init mounts at most `kiln_proto::MAX_LAYERS` (128) layers.
pub const MAX_MAX_LAYERS: u32 = 128;

/// The most app layers Firecracker boots on x86_64 with a network: 17 virtio devices, less the
/// init disk, the scratch disk, vsock and the NIC (kiln-run's `check_devices`). Budgets above it
/// draw a warning, never an error: Cloud Hypervisor (26) and aarch64 Firecracker (88) boot more.
pub const FIRECRACKER_X86_64_LAYERS: u32 = 13;

/// `None` when `n` is a valid budget, else why it is not.
pub fn check_max_layers(n: u32) -> Option<String> {
    (!(MIN_MAX_LAYERS..=MAX_MAX_LAYERS).contains(&n))
        .then(|| format!("max_layers must be {MIN_MAX_LAYERS}-{MAX_MAX_LAYERS}, not {n}"))
}

/// The warning a budget of `n` draws: `Some` above [`FIRECRACKER_X86_64_LAYERS`].
pub fn device_warning(n: u32) -> Option<String> {
    (n > FIRECRACKER_X86_64_LAYERS).then(|| {
        format!(
            "max_layers {n} is over {FIRECRACKER_X86_64_LAYERS}, the most layers Firecracker boots on x86_64 \
             with a network (17 virtio devices); an image that uses the budget needs Cloud Hypervisor \
             (26 layers) or aarch64"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds() {
        for ok in [3, 10, 13, 14, 88, 128] {
            assert_eq!(check_max_layers(ok), None, "{ok}");
        }
        for bad in [0, 1, 2, 129, u32::MAX] {
            let why = check_max_layers(bad).unwrap();
            assert!(why.contains("3-128"), "{why}");
        }
        assert!(check_max_layers(DEFAULT_MAX_LAYERS).is_none());
    }

    #[test]
    fn warns_only_above_the_firecracker_x86_64_ceiling() {
        assert_eq!(device_warning(DEFAULT_MAX_LAYERS), None);
        assert_eq!(device_warning(13), None);
        let w = device_warning(14).unwrap();
        assert!(w.contains("14") && w.contains("Cloud Hypervisor"), "{w}");
    }
}
