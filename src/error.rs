//! Error taxonomy. Every failure maps to one variant with enough context for an
//! operator to act on, and nothing here panics on untrusted input.

/// Errors surfaced by the reconciler.
#[derive(Debug, thiserror::Error)]
pub enum ReconError {
    /// Amount was not a plain decimal string of atomic units.
    #[error("invalid amount: {0}")]
    InvalidAmount(String),

    /// `observed_at` was not RFC 3339.
    #[error("invalid timestamp: {0}")]
    InvalidTimestamp(String),

    /// Configuration is missing or inconsistent; the message names the field.
    #[error("configuration: {0}")]
    Config(String),

    /// A ledger source could not be read or parsed.
    #[error("ledger source: {0}")]
    Ledger(String),

    /// An RPC call failed or returned an unexpected shape.
    #[error("chain RPC: {0}")]
    Chain(String),

    /// Local file I/O failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// JSON parsing failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// HTTP transport failed.
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    /// TOML parsing failed.
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, ReconError>;
