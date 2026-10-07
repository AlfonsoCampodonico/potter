//! Test fixtures: images served by kiln's in-process registry.

use std::io::Write;

use kiln_erofs::testtar::{Opts, TarBuilder};
use kiln_oci::testlayout::{LayoutBuilder, TestLayer};
use kiln_oci::{ContainerConfig, Platform, media};
use kiln_registry::testregistry::{Config, TestRegistry};
use kiln_store::Digest;

/// A one-layer image for the host platform, tagged `tag`, holding `files`, served by a
/// fresh registry. The layer is gzip-compressed, as registries usually serve them.
pub fn registry_with_image(
    tag: &str,
    files: &[(&str, &[u8])],
    env: &[&str],
    workdir: &str,
) -> (TestRegistry, tempfile::TempDir) {
    let mut tar = TarBuilder::new();
    let o = Opts::default();
    for (path, data) in files {
        let mut parent = String::new();
        for part in path
            .split('/')
            .collect::<Vec<_>>()
            .split_last()
            .map(|(_, dirs)| dirs.to_vec())
            .unwrap_or_default()
        {
            parent = if parent.is_empty() {
                part.to_string()
            } else {
                format!("{parent}/{part}")
            };
            tar.dir(&parent, &o);
        }
        tar.file(path, data, &o);
    }
    let raw = tar.finish();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&raw).unwrap();
    let layer = TestLayer {
        media_type: media::OCI_LAYER_GZIP.into(),
        blob: gz.finish().unwrap(),
        diff_id: Digest::of(&raw),
    };
    let dir = tempfile::tempdir().unwrap();
    let mut layout = LayoutBuilder::new(dir.path());
    let config = ContainerConfig {
        env: Some(env.iter().map(|s| s.to_string()).collect()),
        working_dir: Some(workdir.to_string()),
        ..Default::default()
    };
    let image = layout.image(&Platform::host(), &[layer], config);
    layout.add(image, Some(tag));
    let path = layout.finish();
    (TestRegistry::serve_layout(&path, Config::default()), dir)
}
