//! Choosing one provider for a build root (spec §6.1 "Selection").

use std::collections::BTreeSet;

use crate::repo::RepoView;
use crate::{DetectError, ProviderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimKind {
    /// A server-side application (PHP today). It absorbs asset-only JS claims.
    Backend,
    /// A JS app that serves requests (a start script or a server framework).
    JsServer,
    /// JS that only builds assets: a supporting toolchain, or a static site when alone.
    JsAssets,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    pub provider: ProviderId,
    /// The more general provider this one specialises (Laravel refines PHP).
    pub refines: Option<ProviderId>,
    pub kind: ClaimKind,
    pub evidence: Vec<String>,
}

pub fn select(claims: &[Claim], repo: &RepoView) -> Result<ProviderId, DetectError> {
    let refined: BTreeSet<ProviderId> = claims.iter().filter_map(|c| c.refines).collect();
    let mut left: Vec<&Claim> = claims.iter().filter(|c| !refined.contains(&c.provider)).collect();
    if left.iter().any(|c| c.kind == ClaimKind::Backend) {
        left.retain(|c| c.kind != ClaimKind::JsAssets);
    }
    match left.as_slice() {
        [] => Err(DetectError::NoProvider {
            candidates: repo.manifest_dirs(),
        }),
        [one] => Ok(one.provider),
        many => {
            let mut providers: Vec<ProviderId> = many.iter().map(|c| c.provider).collect();
            providers.sort();
            Err(DetectError::Ambiguous { providers })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DetectOptions;

    fn c(provider: ProviderId, refines: Option<ProviderId>, kind: ClaimKind) -> Claim {
        Claim {
            provider,
            refines,
            kind,
            evidence: vec![],
        }
    }

    fn repo() -> (tempfile::TempDir, RepoView) {
        let d = tempfile::tempdir().unwrap();
        let r = RepoView::open(d.path()).unwrap();
        (d, r)
    }

    #[test]
    fn refinement_removes_the_general_claim() {
        let (_d, r) = repo();
        let claims = [
            c(ProviderId::Php, None, ClaimKind::Backend),
            c(ProviderId::Laravel, Some(ProviderId::Php), ClaimKind::Backend),
        ];
        assert_eq!(select(&claims, &r).unwrap(), ProviderId::Laravel);
    }

    #[test]
    fn backends_absorb_asset_only_js() {
        let (_d, r) = repo();
        let claims = [
            c(ProviderId::Php, None, ClaimKind::Backend),
            c(ProviderId::Laravel, Some(ProviderId::Php), ClaimKind::Backend),
            c(ProviderId::Node, None, ClaimKind::JsAssets),
            c(ProviderId::Static, Some(ProviderId::Node), ClaimKind::JsAssets),
        ];
        assert_eq!(select(&claims, &r).unwrap(), ProviderId::Laravel);
    }

    #[test]
    fn a_js_server_competes_with_a_backend() {
        let (_d, r) = repo();
        let claims = [
            c(ProviderId::Laravel, None, ClaimKind::Backend),
            c(ProviderId::Node, None, ClaimKind::JsServer),
        ];
        let e = select(&claims, &r).unwrap_err();
        assert!(
            matches!(&e, DetectError::Ambiguous { providers } if providers == &vec![ProviderId::Laravel, ProviderId::Node])
        );
        assert!(e.to_string().starts_with("laravel, node all claim the build root"));
    }

    #[test]
    fn a_static_site_alone_is_chosen() {
        let (_d, r) = repo();
        let claims = [
            c(ProviderId::Node, None, ClaimKind::JsAssets),
            c(ProviderId::Static, Some(ProviderId::Node), ClaimKind::JsAssets),
        ];
        assert_eq!(select(&claims, &r).unwrap(), ProviderId::Static);
    }

    #[test]
    fn nothing_claims() {
        let (_d, r) = repo();
        assert!(matches!(select(&[], &r), Err(DetectError::NoProvider { .. })));
    }

    #[test]
    fn option_keys() {
        assert_eq!(DetectOptions::default().key("runtime.php"), "target.app.runtime.php");
        let o = DetectOptions {
            key_prefix: "target.api".into(),
            ..Default::default()
        };
        assert_eq!(o.key("port"), "target.api.port");
    }
}
