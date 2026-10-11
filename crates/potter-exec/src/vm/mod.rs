//! `VmExecutor` (spec §9.3): build chains in microVMs. The VM itself comes from kiln's build
//! mode (kiln M3e) behind [`backend::BuildBackend`]; potter owns the disks it attaches, the
//! steps it sends, and the ingest of what comes back, which trusts nothing the guest reports.

pub mod backend;
#[cfg(any(test, feature = "testing"))]
pub mod fake;
pub mod ingest;
