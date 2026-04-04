mod code_action;
mod completion;
mod definition;
mod did_change;
mod did_close;
mod did_open;
mod did_save;
mod forward;
mod hover;
mod initialize;
mod range_formatting;
mod rename;
mod signature_help;
mod spawn_delegate;
mod workspace_edit;

use dashmap::DashMap;
use loom_parse::{CodeChunk, DocumentParser};
use loom_vdoc::VirtualDocument;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::{
    CompletionParams, CompletionResponse, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DidSaveTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverParams, InitializeParams, InitializeResult,
    InitializedParams, MessageType, RenameParams, SignatureHelp, SignatureHelpParams, Url,
};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Range};
use tower_lsp::{LanguageServer, lsp_types};

use crate::registry::DelegateRegistry;

#[derive(Debug)]
pub(crate) struct LoomServer {
    pub(crate) client: tower_lsp::Client,
    pub(crate) chunks: DashMap<Url, Vec<CodeChunk>>,
    pub(crate) virtual_documents: DashMap<Url, Vec<VirtualDocument>>,
    pub(crate) registry: Arc<Mutex<DelegateRegistry>>,
    /// Reverse index: virtual_uri -> (host_uri, VirtualDocument) for O(1) diagnostics lookup.
    pub(crate) reverse_vdoc_index: Arc<DashMap<Url, (Url, VirtualDocument)>>,
    /// Caches the most recent completion result per language. Fast LSPs (pyright) populate this
    /// via direct await; slow LSPs (Julia) populate it via background tasks. Used as fallback
    /// when the direct request times out.
    pub(crate) completion_cache: Arc<DashMap<String, serde_json::Value>>,
    pub(crate) diagnostics_store: Arc<DashMap<Url, HashMap<String, Vec<lsp_types::Diagnostic>>>>,
    pub(crate) parsers: DashMap<Url, Mutex<DocumentParser>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for LoomServer {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        self.handle_initialize(params).await
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        self.handle_shutdown().await
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.handle_did_open(params).await
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.handle_did_close(params).await
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        self.handle_did_change(params).await
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        self.handle_did_save(params).await
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        self.handle_completion(params).await
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Loom server initialized")
            .await;
    }

    async fn hover(&self, params: HoverParams) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        self.handle_hover(params).await
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        self.handle_definition(params).await
    }

    async fn rename(
        &self,
        params: RenameParams,
    ) -> tower_lsp::jsonrpc::Result<Option<tower_lsp::lsp_types::WorkspaceEdit>> {
        self.handle_rename(params).await
    }

    async fn range_formatting(
        &self,
        params: tower_lsp::lsp_types::DocumentRangeFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<lsp_types::TextEdit>>> {
        self.handle_range_formatting(params).await
    }

    async fn code_action(
        &self,
        params: tower_lsp::lsp_types::CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<tower_lsp::lsp_types::CodeActionOrCommand>>> {
        self.handle_code_action(params).await
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SignatureHelp>> {
        self.handle_signature_help(params).await
    }
}

impl LoomServer {
    pub(crate) async fn publish_parse_error(&self, uri: Url, msg: String) {
        self.client
            .publish_diagnostics(
                uri,
                vec![Diagnostic {
                    range: Range::default(),
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("loom".into()),
                    message: msg,
                    ..Default::default()
                }],
                None,
            )
            .await;
    }
}
