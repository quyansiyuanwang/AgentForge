//! Core, target-neutral contracts for AgentForge.

pub mod diagnostic;
pub mod filesystem;
pub mod model;
pub mod validation;

pub use diagnostic::{Diagnostic, DiagnosticCode, Severity};
pub use model::ProjectSpec;
pub use validation::{SpecValidator, ValidationOutcome};
