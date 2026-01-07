use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_char, c_uint, c_ulong, CString};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clang_sys::{
    clang_createIndex, clang_disposeIndex, clang_disposeTranslationUnit, clang_getCursorKind,
    clang_getCursorSpelling, clang_getTranslationUnitCursor, clang_getTokenKind,
    clang_parseTranslationUnit, clang_visitChildren, CXChildVisitResult, CXChildVisit_Recurse,
    CXClientData, CXCursor, CXCursor_MacroExpansion, CXToken_Literal, CXTranslationUnit,
    CXTranslationUnit_DetailedPreprocessingRecord, CXUnsavedFile,
};
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, Diagnostic, DiagnosticSeverity, Position, Range,
};

use crate::compile_commands::CompileCommands;

use super::clang_utils::{
    cursor_range, cxstring_to_string, split_macro_args, token_range, tokenize_cursor,
    tokens_range, tokens_to_string,
};
use super::{range_contains, LspPlugin, DEFAULT_CLANG_ARGS};

pub struct InitDependencyPlugin {
    compile_commands: Option<CompileCommands>,
    targets_by_file: HashMap<PathBuf, Vec<InitTarget>>,
}

#[derive(Clone)]
struct DependencySlot {
    name: String,
    range: Range,
}

#[derive(Clone)]
struct InitTarget {
    name: String,
    stage_expr: String,
    scope_expr: String,
    file: PathBuf,
    dependency_region: Range,
    dependency_slots: Vec<DependencySlot>,
}

impl InitDependencyPlugin {
    pub fn new(project_root: &Path) -> Result<Self> {
        let compile_commands = Some(CompileCommands::load(
            project_root.to_path_buf(),
            DEFAULT_CLANG_ARGS.iter().map(|s| s.to_string()).collect(),
        ));

        Ok(Self {
            compile_commands,
            targets_by_file: HashMap::new(),
        })
    }

    fn iter_targets(&self) -> impl Iterator<Item = &InitTarget> {
        self.targets_by_file.values().flatten()
    }

    fn completion_items(&self) -> Vec<CompletionItem> {
        let mut items: Vec<CompletionItem> = self
            .iter_targets()
            .map(|target| CompletionItem {
                label: target.name.clone(),
                kind: Some(CompletionItemKind::CONSTANT),
                detail: Some(format!("{}/{}", target.stage_expr, target.scope_expr)),
                ..CompletionItem::default()
            })
            .collect();
        items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        items
    }
}

impl LspPlugin for InitDependencyPlugin {
    fn on_file_updated(&mut self, path: &Path, content: Option<&str>) -> Result<()> {
        if path.extension().and_then(|s| s.to_str()) != Some("c") {
            return Ok(());
        }

        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let args = self
            .compile_commands
            .as_ref()
            .map(|db| db.args_for(&canonical))
            .unwrap_or_else(|| DEFAULT_CLANG_ARGS.iter().map(|s| s.to_string()).collect());

        let targets = parse_targets(&canonical, &args, content)?;
        self.targets_by_file.insert(canonical, targets);

        Ok(())
    }

    fn on_file_removed(&mut self, path: &Path) {
        if let Ok(canonical) = path.canonicalize() {
            self.targets_by_file.remove(&canonical);
        }
    }

    fn completions(&self, path: &Path, position: &Position) -> Option<Vec<CompletionItem>> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let targets = self.targets_by_file.get(&canonical)?;
        let in_region = targets
            .iter()
            .any(|target| range_contains(&target.dependency_region, position));

        if !in_region {
            return None;
        }

        Some(self.completion_items())
    }

    fn diagnostics(&self) -> HashMap<PathBuf, Vec<Diagnostic>> {
        let known: BTreeSet<String> = self.iter_targets().map(|t| t.name.clone()).collect();
        let mut diag_map: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();

        for target in self.iter_targets() {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for slot in &target.dependency_slots {
                *counts.entry(slot.name.as_str()).or_default() += 1;
            }

            for slot in &target.dependency_slots {
                if !known.contains(&slot.name) {
                    diag_map
                        .entry(target.file.clone())
                        .or_default()
                        .push(Diagnostic {
                            range: slot.range,
                            severity: Some(DiagnosticSeverity::ERROR),
                            message: format!("Unknown init dependency '{}'", slot.name),
                            source: Some("cronus-init".into()),
                            ..Diagnostic::default()
                        });
                } else if counts[slot.name.as_str()] > 1 {
                    diag_map
                        .entry(target.file.clone())
                        .or_default()
                        .push(Diagnostic {
                            range: slot.range,
                            severity: Some(DiagnosticSeverity::WARNING),
                            message: format!(
                                "Duplicate dependency '{}' in {}",
                                slot.name, target.name
                            ),
                            source: Some("cronus-init".into()),
                            ..Diagnostic::default()
                        });
                }
            }
        }

        diag_map
    }
}

fn parse_targets(path: &Path, args: &[String], content: Option<&str>) -> Result<Vec<InitTarget>> {
    let filename =
        CString::new(path.as_os_str().to_string_lossy().into_owned()).context("path encode")?;
    let arg_cstrings: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_str()))
        .collect::<std::result::Result<_, _>>()?;
    let arg_ptrs: Vec<*const c_char> = arg_cstrings.iter().map(|s| s.as_ptr()).collect();

    let mut unsaved_storage: Vec<CString> = Vec::new();
    let mut unsaved_files: Vec<CXUnsavedFile> = Vec::new();
    if let Some(text) = content {
        let text_c = CString::new(text)?;
        let len = text.len() as c_ulong;
        unsaved_storage.push(text_c);
        unsaved_files.push(CXUnsavedFile {
            Filename: filename.as_ptr(),
            Contents: unsaved_storage.last().unwrap().as_ptr(),
            Length: len,
        });
    }

    unsafe {
        let index = clang_createIndex(0, 0);
        let tu = clang_parseTranslationUnit(
            index,
            filename.as_ptr(),
            if arg_ptrs.is_empty() {
                std::ptr::null()
            } else {
                arg_ptrs.as_ptr()
            },
            arg_ptrs.len() as c_uint as i32,
            if unsaved_files.is_empty() {
                std::ptr::null_mut()
            } else {
                unsaved_files.as_mut_ptr()
            },
            unsaved_files.len() as c_uint,
            CXTranslationUnit_DetailedPreprocessingRecord,
        );

        if tu.is_null() {
            clang_disposeIndex(index);
            return Err(anyhow!("Unable to parse {} with libclang", path.display()));
        }

        let cursor = clang_getTranslationUnitCursor(tu);
        let mut collector = TargetCollector {
            tu,
            file: path.to_path_buf(),
            targets: Vec::new(),
        };

        clang_visitChildren(
            cursor,
            visit_targets,
            &mut collector as *mut TargetCollector as CXClientData,
        );

        clang_disposeTranslationUnit(tu);
        clang_disposeIndex(index);
        Ok(collector.targets)
    }
}

struct TargetCollector {
    tu: CXTranslationUnit,
    file: PathBuf,
    targets: Vec<InitTarget>,
}

extern "C" fn visit_targets(
    cursor: CXCursor,
    _parent: CXCursor,
    data: CXClientData,
) -> CXChildVisitResult {
    unsafe {
        let collector = &mut *(data as *mut TargetCollector);
        if clang_getCursorKind(cursor) == CXCursor_MacroExpansion {
            let spelling = cxstring_to_string(clang_getCursorSpelling(cursor));
            if spelling == "INIT_TARGET" {
                if let Some(target) = build_target(collector, cursor) {
                    collector.targets.push(target);
                }
            }
        }
        CXChildVisit_Recurse
    }
}

unsafe fn build_target(collector: &TargetCollector, cursor: CXCursor) -> Option<InitTarget> {
    let tokens = tokenize_cursor(collector.tu, cursor)?;
    let args = split_macro_args(collector.tu, &tokens)?;
    if args.len() != 4 {
        return None;
    }
    let name = tokens_to_string(collector.tu, &args[0])?;
    let stage_expr = tokens_to_string(collector.tu, &args[1])?;
    let scope_expr = tokens_to_string(collector.tu, &args[2])?;
    let deps_tokens = &args[3];
    let mut dependency_region =
        tokens_range(collector.tu, deps_tokens).or_else(|| cursor_range(cursor))?;
    let mut dependency_slots = Vec::new();
    for token in deps_tokens {
        if clang_getTokenKind(*token) == CXToken_Literal {
            let literal_range = token_range(collector.tu, *token)?;
            dependency_region.end = literal_range.end;
            let literal = tokens_to_string(collector.tu, &[*token])?;
            let name = literal.trim_matches('"').to_string();
            dependency_slots.push(DependencySlot {
                name,
                range: literal_range,
            });
        }
    }
    Some(InitTarget {
        name,
        stage_expr,
        scope_expr,
        file: collector.file.clone(),
        dependency_region,
        dependency_slots,
    })
}
