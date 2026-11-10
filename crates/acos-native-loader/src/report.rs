//! Structured, serialisable loader reports.

use serde::Serialize;

/// Top-level report returned by [`crate::Loader::report`].
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub bionic_table: TableReport,
    pub libraries: Vec<LibraryReport>,
}

/// Summary of the bionic compatibility table.
#[derive(Debug, Clone, Serialize)]
pub struct TableReport {
    pub forwarded: usize,
    pub shimmed: usize,
    pub total: usize,
}

/// Per-library import coverage.
#[derive(Debug, Clone, Serialize)]
pub struct LibraryReport {
    pub name: String,
    pub path: Option<String>,
    pub imports_total: usize,
    pub imports_forwarded: usize,
    pub imports_shimmed: usize,
    pub imports_missing: Vec<String>,
    /// Number of imports that carried a symbol version.
    pub imports_versioned: usize,
    pub relocations: usize,
    pub tls: bool,
    pub deps: Vec<String>,
}

impl Report {
    /// Render as pretty JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}
