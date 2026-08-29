//! Vertical #1 — LocalFS. Stores notes as .eml files (RFC822 wrapping the same
//! Apple-HTML body as Gmail) under a root directory. Reuses mime822 (encode),
//! the Apple-HTML content model + editor, and Identity (X-UUID in the file). The
//! one new piece is raw-RFC822 decode (decode.rs).

use super::{Capabilities, ContentKind, Derived, Deriver, Identity, Vertical};

pub mod decode;
pub mod transport;

pub struct LocalFsVertical {
    pub(crate) root: std::path::PathBuf,
    #[allow(dead_code)]
    pub(crate) account_id: String,
    capabilities: Capabilities,
}

impl LocalFsVertical {
    pub fn new(root: std::path::PathBuf, account_id: String) -> Self {
        Self { root, account_id, capabilities: Capabilities::for_backend(crate::accounts::BackendKind::LocalFs) }
    }
    pub(crate) fn notes_dir(&self) -> std::path::PathBuf { self.root.join("Notes") }
    pub(crate) fn trash_dir(&self) -> std::path::PathBuf { self.root.join(".trash") }
    pub(crate) fn meta_dir(&self) -> std::path::PathBuf { self.root.join(".meta") }
}

impl Identity for LocalFsVertical {
    fn mint(&self) -> String { crate::mime822::format_apple_uuid(uuid::Uuid::new_v4()) }
}
impl Deriver for LocalFsVertical {
    fn derive(&self, kind: ContentKind, blob: &[u8]) -> Derived {
        crate::backend::deriver_applehtml::AppleHtmlDeriver.derive(kind, blob)
    }
}

impl Vertical for LocalFsVertical {
    fn backend_id(&self) -> &str { "localfs" }
    fn capabilities(&self) -> &Capabilities { &self.capabilities }
}
