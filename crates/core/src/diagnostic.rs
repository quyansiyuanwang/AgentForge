use serde::{Deserialize, Serialize};

/// Stable, machine-readable diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// Stable diagnostic identifiers. Variants may be added, but existing codes do not change meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticCode {
    #[serde(rename = "AF1001")]
    InvalidYaml,
    #[serde(rename = "AF1002")]
    SchemaViolation,
    #[serde(rename = "AF1101")]
    GenericTargetConflict,
    #[serde(rename = "AF1102")]
    DuplicateId,
    #[serde(rename = "AF1103")]
    UnknownReference,
    #[serde(rename = "AF1104")]
    TargetNotSelected,
    #[serde(rename = "AF1105")]
    ExtensionTargetMismatch,
    #[serde(rename = "AF1106")]
    InvalidSourceKind,
    #[serde(rename = "AF1107")]
    InsecureSecretValue,
    #[serde(rename = "AF1108")]
    UnsafePath,
    #[serde(rename = "AF1109")]
    UndeclaredEnvironmentVariable,
    #[serde(rename = "AF1110")]
    UrlContainsCredentials,
    #[serde(rename = "AF1201")]
    UnsupportedCapability,
    #[serde(rename = "AF1202")]
    ManualActionRequired,
    #[serde(rename = "AF1301")]
    ArtifactConflict,
    #[serde(rename = "AF1302")]
    ArtifactDrift,
    #[serde(rename = "AF1303")]
    DuplicateArtifactOwner,
    #[serde(rename = "AF2001")]
    DetectorFailure,
    #[serde(rename = "AF2002")]
    MalformedDetectionInput,
    #[serde(rename = "AF3001")]
    IoFailure,
}

impl DiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidYaml => "AF1001",
            Self::SchemaViolation => "AF1002",
            Self::GenericTargetConflict => "AF1101",
            Self::DuplicateId => "AF1102",
            Self::UnknownReference => "AF1103",
            Self::TargetNotSelected => "AF1104",
            Self::ExtensionTargetMismatch => "AF1105",
            Self::InvalidSourceKind => "AF1106",
            Self::InsecureSecretValue => "AF1107",
            Self::UnsafePath => "AF1108",
            Self::UndeclaredEnvironmentVariable => "AF1109",
            Self::UrlContainsCredentials => "AF1110",
            Self::UnsupportedCapability => "AF1201",
            Self::ManualActionRequired => "AF1202",
            Self::ArtifactConflict => "AF1301",
            Self::ArtifactDrift => "AF1302",
            Self::DuplicateArtifactOwner => "AF1303",
            Self::DetectorFailure => "AF2001",
            Self::MalformedDetectionInput => "AF2002",
            Self::IoFailure => "AF3001",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Diagnostic {
    pub fn error(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            path: None,
            target: None,
            remediation: None,
        }
    }

    pub fn warning(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Warning,
            message: message.into(),
            path: None,
            target: None,
            remediation: None,
        }
    }

    pub fn at_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn for_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    pub fn with_remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = Some(remediation.into());
        self
    }
}
