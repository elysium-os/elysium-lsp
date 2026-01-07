use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_char, c_uint, c_ulong, CString};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use clang_sys::{
    clang_createIndex, clang_disposeIndex, clang_disposeTranslationUnit, clang_getCursorKind,
    clang_getCursorSpelling, clang_getTranslationUnitCursor, clang_parseTranslationUnit,
    clang_visitChildren, CXChildVisitResult, CXChildVisit_Recurse, CXClientData, CXCursor,
    CXCursor_MacroExpansion, CXToken, CXTranslationUnit, CXTranslationUnit_DetailedPreprocessingRecord,
    CXUnsavedFile,
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

pub struct HookPlugin {
    compile_commands: Option<CompileCommands>,
    files: HashMap<PathBuf, HookFileData>,
}

#[derive(Default)]
struct HookFileData {
    definitions: Vec<HookDefinition>,
    invocations: Vec<HookInvocation>,
}

#[derive(Clone)]
struct HookDefinition {
    name: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HookInvocationKind {
    Definition,
    Run,
}

#[derive(Clone)]
struct HookInvocation {
    name: String,
    name_range: Range,
    argument_region: Range,
    kind: HookInvocationKind,
}

impl HookPlugin {
    pub fn new(project_root: &Path) -> Result<Self> {
        let compile_commands = Some(CompileCommands::load(
            project_root.to_path_buf(),
            DEFAULT_CLANG_ARGS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        ));

        Ok(Self {
            compile_commands,
            files: HashMap::new(),
        })
    }

    fn iter_definitions(&self) -> impl Iterator<Item = &HookDefinition> {
        self.files.values().flat_map(|data| data.definitions.iter())
    }

    fn completion_items(&self) -> Vec<CompletionItem> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        for definition in self.iter_definitions() {
            names.insert(definition.name.clone());
        }

        names
            .into_iter()
            .map(|name| CompletionItem {
                label: name,
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some("hook".into()),
                ..CompletionItem::default()
            })
            .collect()
    }
}

impl LspPlugin for HookPlugin {
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

        let data = parse_hooks(&canonical, &args, content)?;
        self.files.insert(canonical, data);
        Ok(())
    }

    fn on_file_removed(&mut self, path: &Path) {
        if let Ok(canonical) = path.canonicalize() {
            self.files.remove(&canonical);
        }
    }

    fn completions(&self, path: &Path, position: &Position) -> Option<Vec<CompletionItem>> {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let data = self.files.get(&canonical)?;
        let in_region = data
            .invocations
            .iter()
            .any(|invocation| range_contains(&invocation.argument_region, position));

        if !in_region {
            return None;
        }

        Some(self.completion_items())
    }

    fn diagnostics(&self) -> HashMap<PathBuf, Vec<Diagnostic>> {
        let known: BTreeSet<String> = self.iter_definitions().map(|d| d.name.clone()).collect();
        let mut diag_map: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();

        for (file, data) in &self.files {
            for invocation in data
                .invocations
                .iter()
                .filter(|invocation| invocation.kind == HookInvocationKind::Run)
            {
                if invocation.name.is_empty() {
                    continue;
                }

                if !known.contains(&invocation.name) {
                    diag_map.entry(file.clone()).or_default().push(Diagnostic {
                        range: invocation.name_range.clone(),
                        severity: Some(DiagnosticSeverity::ERROR),
                        message: format!("Unknown hook '{}'", invocation.name),
                        source: Some("cronus-hooks".into()),
                        ..Diagnostic::default()
                    });
                }
            }
        }

        diag_map
    }
}

fn parse_hooks(path: &Path, args: &[String], content: Option<&str>) -> Result<HookFileData> {
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
        let mut collector = HookCollector {
            tu,
            definitions: Vec::new(),
            invocations: Vec::new(),
        };

        clang_visitChildren(
            cursor,
            visit_hooks,
            &mut collector as *mut HookCollector as CXClientData,
        );

        clang_disposeTranslationUnit(tu);
        clang_disposeIndex(index);
        Ok(HookFileData {
            definitions: collector.definitions,
            invocations: collector.invocations,
        })
    }
}

struct HookCollector {
    tu: CXTranslationUnit,
    definitions: Vec<HookDefinition>,
    invocations: Vec<HookInvocation>,
}

extern "C" fn visit_hooks(
    cursor: CXCursor,
    _parent: CXCursor,
    data: CXClientData,
) -> CXChildVisitResult {
    unsafe {
        let collector = &mut *(data as *mut HookCollector);
        if clang_getCursorKind(cursor) == CXCursor_MacroExpansion {
            let spelling = cxstring_to_string(clang_getCursorSpelling(cursor));
            match spelling.as_str() {
                "HOOK" => {
                    if let Some(definition) = build_hook_definition(collector, cursor) {
                        collector.definitions.push(definition);
                    }
                    if let Some(invocation) =
                        build_hook_usage(collector, cursor, HookInvocationKind::Definition)
                    {
                        collector.invocations.push(invocation);
                    }
                }
                "HOOK_RUN" => {
                    if let Some(invocation) =
                        build_hook_usage(collector, cursor, HookInvocationKind::Run)
                    {
                        collector.invocations.push(invocation);
                    }
                }
                _ => {}
            }
        }
        CXChildVisit_Recurse
    }
}

unsafe fn build_hook_definition(
    collector: &HookCollector,
    cursor: CXCursor,
) -> Option<HookDefinition> {
    let tokens = tokenize_cursor(collector.tu, cursor)?;
    let args = split_macro_args(collector.tu, &tokens)?;
    if args.len() != 1 {
        return None;
    }
    let name = tokens_to_string(collector.tu, &args[0])?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(HookDefinition { name })
}

unsafe fn build_hook_usage(
    collector: &HookCollector,
    cursor: CXCursor,
    kind: HookInvocationKind,
) -> Option<HookInvocation> {
    let tokens = tokenize_cursor(collector.tu, cursor)?;
    let args = split_macro_args(collector.tu, &tokens)?;
    if args.len() != 1 {
        return None;
    }

    let argument_region =
        macro_argument_region(collector.tu, &tokens).or_else(|| cursor_range(cursor))?;
    let name_tokens = &args[0];
    let (name, name_range) = if name_tokens.is_empty() {
        (String::new(), argument_region.clone())
    } else {
        let name = tokens_to_string(collector.tu, name_tokens)?.trim().to_string();
        let range = tokens_range(collector.tu, name_tokens).unwrap_or_else(|| argument_region.clone());
        (name, range)
    };

    Some(HookInvocation {
        name,
        name_range,
        argument_region,
        kind,
    })
}

unsafe fn macro_argument_region(tu: CXTranslationUnit, tokens: &[CXToken]) -> Option<Range> {
    let mut depth = 0;
    let mut start = None;
    for token in tokens {
        let text = tokens_to_string(tu, &[*token]).unwrap_or_default();
        match text.as_str() {
            "(" => {
                if depth == 0 {
                    start = Some(token_range(tu, *token)?.end);
                }
                depth += 1;
            }
            ")" => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    let end = token_range(tu, *token)?.start;
                    if let Some(start_pos) = start {
                        return Some(Range {
                            start: start_pos,
                            end,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    None
}
