use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tower_lsp::lsp_types::{CompletionItem, Diagnostic, Position};

pub trait LspPlugin: Send + Sync {
    fn on_file_updated(&mut self, path: &Path, content: Option<&str>) -> Result<()>;
    fn on_file_removed(&mut self, path: &Path);
    fn completions(&self, path: &Path, position: &Position) -> Option<Vec<CompletionItem>>;
    fn diagnostics(&self) -> HashMap<PathBuf, Vec<Diagnostic>>;
}

pub mod init;
pub use init::InitDependencyPlugin;
