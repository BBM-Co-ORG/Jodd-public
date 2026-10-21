use crate::backend::{ContentKind, Derived, Deriver, Edge, TagModel};

/// Deriver for the Apple-HTML content model, shared by every vertical that
/// stores Apple-HTML bodies. Delegates to the same pure db.rs body parsers the
/// write path uses, so the neutral view matches the index — including what
/// counts as a tag, which differs by backend (`TagModel`, gotcha #38), so each
/// vertical builds it with [`AppleHtmlDeriver::for_backend`].
pub struct AppleHtmlDeriver {
    pub tag_model: TagModel,
}

impl AppleHtmlDeriver {
    /// The deriver for `kind`'s notes — the one place a vertical's backend
    /// becomes its tag rule.
    pub fn for_backend(kind: crate::accounts::BackendKind) -> Self {
        AppleHtmlDeriver { tag_model: TagModel::for_backend(kind) }
    }
}

impl Deriver for AppleHtmlDeriver {
    fn derive(&self, _kind: ContentKind, blob: &[u8]) -> Derived {
        let body = std::str::from_utf8(blob).unwrap_or("");
        let text = crate::db::strip_html_to_text(body);
        let tags = crate::db::tags_from_body_for(self.tag_model, body);
        let edges = crate::db::extract_wikilinks(body)
            .into_iter().map(|target| Edge { rel: "mentions".to_string(), target }).collect();
        Derived { text, tags, edges }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{BackendKind, ALL_BACKENDS};
    use std::sync::Arc;

    /// One real iCloud tag (the span) and one `#word` in the text — what
    /// Apple's Delete Tag leaves behind (gotcha #38).
    const SPAN_AND_TEXT: &str = r#"<div>t</div><div><span data-jodd-inline="hashtag" data-ref="ia-1" contenteditable="false">#LabLower</span> #plain</div>"#;

    /// Each backend's vertical, built without touching the network. The match
    /// has no wildcard arm, so a sixth backend cannot skip this test.
    fn vertical_of(kind: BackendKind) -> Box<dyn Deriver> {
        match kind {
            BackendKind::Gmail => Box::new(crate::backend::gmail::GmailVertical::new(
                String::new(), Default::default(), "a@gmail.com".into(), String::new(),
            )),
            BackendKind::Microsoft => Box::new(crate::backend::microsoft::MicrosoftVertical::new(
                String::new(), "microsoft:a@live.com".into(), Default::default(),
            )),
            BackendKind::LocalFs => Box::new(crate::backend::localfs::LocalFsVertical::new(
                std::env::temp_dir(), "localfs:a".into(),
            )),
            BackendKind::Ssh => {
                use crate::backend::ssh::session::{ProcessSession, SpawnSpec};
                let spec = SpawnSpec { program: "ssh".into(), args: Vec::new(), cwd: None };
                let session = Arc::new(ProcessSession::new(spec, std::time::Duration::from_secs(1)));
                Box::new(crate::backend::ssh::SshVertical::new(session, "host".into(), "/r".into(), "ssh:a".into()))
            }
            BackendKind::ICloud => {
                use crate::icloud_auth::{ClientConfig, IcloudSession, StaticJar};
                Box::new(crate::backend::icloud::ICloudVertical::new(
                    IcloudSession {
                        apple_id: "a@me.com".into(),
                        dsid: "1".into(),
                        ck_host: "http://127.0.0.1".into(),
                        client: ClientConfig {
                            client_build_number: String::new(),
                            client_mastering_number: String::new(),
                            client_id: String::new(),
                        },
                    },
                    Arc::new(StaticJar(Vec::new())),
                    "icloud:a@me.com".into(),
                    Arc::new(crate::backend::icloud::AccountCache::default()),
                    [0; 16],
                ))
            }
        }
    }

    fn tags(d: &dyn Deriver) -> Vec<String> {
        d.derive(ContentKind::AppleHtml, SPAN_AND_TEXT.as_bytes()).tags
    }

    /// Every vertical reads tags under its OWN backend's `TagModel` — iCloud
    /// ignores a plain `#word`, the rest count it.
    #[test]
    fn each_vertical_derives_tags_under_its_own_backends_model() {
        for kind in ALL_BACKENDS {
            let want: &[&str] = match TagModel::for_backend(kind) {
                TagModel::InlineObjects => &["lablower"],
                TagModel::BodyText => &["lablower", "plain"],
            };
            assert_eq!(tags(vertical_of(kind).as_ref()), want, "{kind:?}'s vertical");
            assert_eq!(tags(&AppleHtmlDeriver::for_backend(kind)), want, "{kind:?}'s deriver");
        }
        assert_eq!(tags(vertical_of(BackendKind::ICloud).as_ref()), ["lablower"]);
        assert_eq!(tags(vertical_of(BackendKind::Gmail).as_ref()), ["lablower", "plain"]);
    }
}
