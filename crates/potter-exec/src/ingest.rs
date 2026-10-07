//! The bounded ingest of untrusted layer tars (spec §9.4 sealing, P4): a diff is converted
//! with kiln-erofs and its limits before it is stored or cached, so a layer that kiln would
//! refuse never enters the cache.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use kiln_erofs::{LayerWriter, Limits};

use crate::error::{BuildError, io};

/// Checks `tar` the way kiln will ingest it: kiln-erofs's conversion under its default limits.
/// A tar it refuses is a violation (exit 3).
pub fn validate_layer(tar: impl Read, spill_dir: &Path) -> Result<(), BuildError> {
    let out = tempfile::tempfile_in(spill_dir).map_err(io("creating the ingest check's output"))?;
    let mut writer = LayerWriter::new(out, spill_dir, Limits::default())
        .map_err(|e| BuildError::Infra(format!("checking a layer: {e}")))?;
    writer
        .append_tar(tar)
        .map_err(|e| BuildError::Hostile(format!("the layer does not ingest: {e}")))?;
    writer
        .finish(&BTreeMap::new())
        .map_err(|e| BuildError::Hostile(format!("the layer does not ingest: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use kiln_erofs::testtar::{Opts, TarBuilder};

    use super::*;

    #[test]
    fn a_valid_layer_passes_and_a_forward_hardlink_is_refused() {
        let o = Opts::default();
        let spill = tempfile::tempdir().unwrap();
        let mut ok = TarBuilder::new();
        ok.dir("d", &o).file("d/x", b"x", &o).hardlink("d/y", "d/x");
        validate_layer(std::io::Cursor::new(ok.finish()), spill.path()).unwrap();
        let mut forward = TarBuilder::new();
        forward.dir("d", &o).hardlink("d/a", "d/x").file("d/x", b"x", &o);
        let err = validate_layer(std::io::Cursor::new(forward.finish()), spill.path()).unwrap_err();
        assert_eq!(err.exit_code(), 3, "{err}");
    }
}
