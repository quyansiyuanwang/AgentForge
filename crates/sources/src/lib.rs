//! Remote source resolution, lock contracts, and vendored-content verification.

pub mod adapters;
pub mod lock;
pub mod offline;
pub mod security;
pub mod service;

pub use lock::{ContentKind, LockEntry, LockFile, SourceType};
pub use offline::verify_vendor_offline;
pub use security::{EntryKind, UntrustedEntry, VendorLimits, VendorTree};
pub use service::{ResolveRequest, ResolvedSource, SourceError, SourceService};
