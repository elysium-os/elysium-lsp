use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionResponse, Diagnostic, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    FileChangeType, InitializeParams, InitializeResult, InitializedParams, Position,
    ServerCapabilities, TextDocumentContentChangeEvent, TextDocumentSyncCapability,
    TextDocumentSyncKind,
};
use tower_lsp::{Client, LanguageServer};
use walkdir::WalkDir;

use crate::plugins::LspPlugin;

struct State {
    project_root: PathBuf,
    documents: HashMap<tower_lsp::lsp_types::Url, String>,
    plugins: Vec<Box<dyn LspPlugin>>,
    published_paths: HashSet<PathBuf>,
}

pub struct ElysiumLsp {
    client: Client,
    state: Arc<Mutex<State>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for ElysiumLsp {
    async fn initialize(&self, _: InitializeParams) -> LspResult<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(Default::default()),
                ..ServerCapabilities::default()
            },
            ..InitializeResult::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        let mut state = self.state.lock().await;
        for entry in WalkDir::new(&state.project_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if let Err(err) = state.file_updated(entry.path(), None) {
                fatal_parse_error(&err);
            }
        }
        drop(state);

        self.publish_all_diagnostics().await;
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let text = params.text_document.text;

        let mut state = self.state.lock().await;
        state.documents.insert(uri.clone(), text.clone());
        drop(state);

        if let Err(err) = self.reindex(&uri, Some(text)).await {
            fatal_parse_error(&err);
        }
        self.publish_all_diagnostics().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();

        let mut latest = None;
        for change in params.content_changes {
            latest = Some(change);
        }

        if let Some(TextDocumentContentChangeEvent { text, .. }) = latest {
            let mut state = self.state.lock().await;
            state.documents.insert(uri.clone(), text.clone());
            drop(state);

            if let Err(err) = self.reindex(&uri, Some(text)).await {
                fatal_parse_error(&err);
            }
            self.publish_all_diagnostics().await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;

        let mut state = self.state.lock().await;
        state.documents.remove(&uri);
        drop(state);

        if let Err(err) = self.reindex(&uri, None).await {
            fatal_parse_error(&err);
        }
        self.publish_all_diagnostics().await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            if let Ok(path) = change.uri.to_file_path() {
                let result = {
                    let mut state = self.state.lock().await;
                    match change.typ {
                        FileChangeType::DELETED => {
                            state.file_removed(&path);
                            Ok(())
                        }
                        _ => state.file_updated(&path, None),
                    }
                };
                if let Err(err) = result {
                    fatal_parse_error(&err);
                }
            }
        }

        self.publish_all_diagnostics().await;
    }

    async fn completion(
        &self,
        params: tower_lsp::lsp_types::CompletionParams,
    ) -> LspResult<Option<CompletionResponse>> {
        let path = match params
            .text_document_position
            .text_document
            .uri
            .to_file_path()
        {
            Ok(path) => path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
            Err(_) => return Ok(None),
        };

        let state = self.state.lock().await;
        if let Some(items) = state.completions(&path, &params.text_document_position.position) {
            return Ok(Some(CompletionResponse::Array(items)));
        }

        Ok(None)
    }
}

impl ElysiumLsp {
    pub fn new(client: Client, project_root: PathBuf, plugins: Vec<Box<dyn LspPlugin>>) -> Self {
        Self {
            client,
            state: Arc::new(Mutex::new(State::new(project_root, plugins))),
        }
    }

    async fn reindex(
        &self,
        uri: &tower_lsp::lsp_types::Url,
        content: Option<String>,
    ) -> Result<()> {
        let path = uri
            .to_file_path()
            .map_err(|_| anyhow!("URI is not a local file"))?;

        self.state
            .lock()
            .await
            .file_updated(&path, content.as_deref())
    }

    async fn publish_all_diagnostics(&self) {
        let (diagnostics, published_paths) = {
            let state = self.state.lock().await;
            (state.diagnostics(), state.published_paths.clone())
        };
        let current_paths = diagnostics.keys().cloned().collect();

        for (path, diagnostics) in diagnostics {
            if let Ok(uri) = tower_lsp::lsp_types::Url::from_file_path(&path) {
                self.client
                    .publish_diagnostics(uri, diagnostics, None)
                    .await;
            }
        }

        let stale: Vec<PathBuf> = published_paths
            .difference(&current_paths)
            .cloned()
            .collect();
        for path in stale {
            if let Ok(uri) = tower_lsp::lsp_types::Url::from_file_path(&path) {
                self.client.publish_diagnostics(uri, vec![], None).await;
            }
        }

        self.state.lock().await.published_paths = current_paths;
    }
}

impl State {
    fn new(project_root: PathBuf, plugins: Vec<Box<dyn LspPlugin>>) -> Self {
        Self {
            project_root,
            documents: HashMap::new(),
            plugins,
            published_paths: HashSet::new(),
        }
    }

    fn file_updated(&mut self, path: &Path, content: Option<&str>) -> Result<()> {
        for plugin in &mut self.plugins {
            plugin.on_file_updated(path, content)?;
        }
        Ok(())
    }

    fn file_removed(&mut self, path: &Path) {
        for plugin in &mut self.plugins {
            plugin.on_file_removed(path);
        }
    }

    fn diagnostics(&self) -> HashMap<PathBuf, Vec<Diagnostic>> {
        let mut all: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
        for plugin in &self.plugins {
            for (path, diagnostics) in plugin.diagnostics() {
                all.entry(path).or_default().extend(diagnostics.into_iter());
            }
        }
        all
    }

    fn completions(&self, path: &Path, position: &Position) -> Option<Vec<CompletionItem>> {
        for plugin in &self.plugins {
            if let Some(items) = plugin.completions(path, position) {
                return Some(items);
            }
        }
        None
    }
}

fn fatal_parse_error(err: &anyhow::Error) -> ! {
    eprintln!("elysium-lsp fatal error: {err:?}");
    process::exit(1);
}
