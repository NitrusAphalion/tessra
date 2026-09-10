//! The operation log: `spec/03-operations.md` and the verification algorithm
//! of `spec/04-security.md`.
//!
//! A view is the mutable state at one op. Effects move a view forward.
//! Views merge three-way. Every op is verified against the merged view of its
//! parents and, for revocation and rotation, against the receiver's current
//! view. Nothing here touches a clock.

pub mod build;
pub mod dag;
pub mod glob;
pub mod standard;
pub mod verify;
pub mod view;

pub use dag::OpLog;
pub use view::ViewState;

/// Errors raised by the operation log.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] tessra_core::Error),
    #[error("rejected at step {step} ({name}): {reason}")]
    Rejected {
        step: u8,
        name: &'static str,
        reason: String,
    },
    #[error("unknown parent op {0}")]
    UnknownParent(tessra_core::ObjectId),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn rejected(step: u8, name: &'static str, reason: impl Into<String>) -> Self {
        Error::Rejected {
            step,
            name,
            reason: reason.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
