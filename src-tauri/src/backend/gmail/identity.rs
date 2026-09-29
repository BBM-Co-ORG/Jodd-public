use crate::backend::Identity;
use super::GmailVertical;

impl Identity for GmailVertical {
    fn mint(&self) -> String {
        crate::mime822::format_apple_uuid(uuid::Uuid::new_v4())
    }
}
