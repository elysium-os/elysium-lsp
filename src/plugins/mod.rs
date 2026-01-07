use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tower_lsp::lsp_types::{CompletionItem, Diagnostic, Position, Range};

pub(crate) const DEFAULT_CLANG_ARGS: &[&str] = &["-Iinclude", "-std=gnu23"];

pub trait LspPlugin: Send + Sync {
    fn on_file_updated(&mut self, path: &Path, content: Option<&str>) -> Result<()>;
    fn on_file_removed(&mut self, path: &Path);
    fn completions(&self, path: &Path, position: &Position) -> Option<Vec<CompletionItem>>;
    fn diagnostics(&self) -> HashMap<PathBuf, Vec<Diagnostic>>;
}

pub(crate) fn range_contains(range: &Range, pos: &Position) -> bool {
    if pos.line < range.start.line || pos.line > range.end.line {
        return false;
    }
    if pos.line == range.start.line && pos.character < range.start.character {
        return false;
    }
    if pos.line == range.end.line && pos.character > range.end.character {
        return false;
    }
    true
}

mod clang_utils;

pub mod init;
pub mod hooks;
pub use hooks::HookPlugin;
pub use init::InitDependencyPlugin;
