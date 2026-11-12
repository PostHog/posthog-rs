//! Transactional email: the public types, the I/O-free request and reply
//! handling in `shared`, and one `Emails` handle per HTTP client flavor.

mod error;
mod shared;
mod types;

#[cfg(feature = "async-client")]
mod async_emails;
#[cfg(not(feature = "async-client"))]
mod blocking_emails;

#[cfg(feature = "async-client")]
pub use async_emails::Emails;
#[cfg(not(feature = "async-client"))]
pub use blocking_emails::Emails;

pub use error::{EmailError, EmailErrorKind};
pub(crate) use shared::EmailTracker;
pub use types::{
    CodeEmail, EmailCategory, EmailDeliveryStatus, EmailStatus, EmailWarning, MessageCategory,
    MessageCategoryType, SendEmail, SendEmailResult, TemplateEmail,
};
