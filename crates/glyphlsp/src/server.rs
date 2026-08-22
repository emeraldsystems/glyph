use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::Mutex;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};

use glyph_frontend::{FrontendOptions, compile_source};

use crate::diagnostics::glyph_diagnostic_to_lsp;
use crate::document::DocumentState;
#[cfg(test)]
use crate::document::build_line_starts;

fn compile_diagnostics(source: &str, line_starts: &[u32]) -> Vec<Diagnostic> {
    let output = compile_source(
        source,
        FrontendOptions {
            include_std: true,
            ..Default::default()
        },
    );

    output
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.module_id.as_deref() == Some("main") || diagnostic.module_id.is_none()
        })
        .map(|diagnostic| glyph_diagnostic_to_lsp(diagnostic, source, line_starts))
        .collect()
}

pub struct GlyphLanguageServer {
    pub client: Client,
    pub documents: DashMap<Url, DocumentState>,
    debounce_tokens: Arc<Mutex<u64>>,
}

impl GlyphLanguageServer {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: DashMap::new(),
            debounce_tokens: Arc::new(Mutex::new(0)),
        }
    }

    async fn compile_and_publish(&self, uri: Url) {
        let (source, line_starts) = {
            let doc = match self.documents.get(&uri) {
                Some(doc) => doc,
                None => return,
            };
            (doc.source.clone(), doc.line_starts.clone())
        };

        let lsp_diags =
            tokio::task::spawn_blocking(move || compile_diagnostics(&source, &line_starts))
                .await
                .unwrap_or_default();

        self.client.publish_diagnostics(uri, lsp_diags, None).await;
    }

    async fn debounced_compile(&self, uri: Url) {
        let token = {
            let mut t = self.debounce_tokens.lock().await;
            *t += 1;
            *t
        };

        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let current = *self.debounce_tokens.lock().await;
        if current != token {
            return; // a newer change superseded this one
        }

        self.compile_and_publish(uri).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_owned_closure_has_no_lsp_diagnostics() {
        let source = r#"
fn main() -> i32 {
  let offset: i32 = 40
  let add: FnOnce<i32, i32> = value -> offset + value
  ret add(2)
}
"#;
        let diagnostics = compile_diagnostics(source, &build_line_starts(source));

        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
    }

    #[test]
    fn valid_borrowed_callables_have_no_lsp_diagnostics() {
        let source = r#"
struct Counter { value: i32 }

fn main() -> i32 {
  let offset: i32 = 40
  let add: Fn<i32, i32> = (value: i32) -> offset + value
  let mut counter: Counter = Counter { value: 0 }
  let mut next: FnMut<(), i32> = () -> {
    counter.value = counter.value + 1
    counter.value
  }
  ret add(next())
}
"#;
        let diagnostics = compile_diagnostics(source, &build_line_starts(source));

        assert!(diagnostics.is_empty(), "diagnostics: {diagnostics:?}");
    }

    #[test]
    fn scoped_detach_error_is_published_with_a_source_range() {
        let source = r#"import ScopedJoinHandle from std/thread

fn invalid(handle: ScopedJoinHandle<i32>) {
  handle.detach()
}

fn main() -> i32 { ret 0 }
"#;
        let diagnostics = compile_diagnostics(source, &build_line_starts(source));

        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.message.contains("cannot be detached"))
            .expect("expected the scoped detach diagnostic");
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diagnostic.source.as_deref(), Some("glyph"));
        assert_eq!(diagnostic.range.start.line, 3);
        assert!(diagnostic.range.end.character > diagnostic.range.start.character);
    }

    #[test]
    fn closure_error_is_published_with_a_source_range() {
        let source = r#"fn main() -> i32 {
  let add: FnOnce<(i32, i32), i32> = x, y -> x + y
  ret 0
}
"#;
        let diagnostics = compile_diagnostics(source, &build_line_starts(source));

        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic
                    .message
                    .contains("multiple closure parameters require parentheses")
            })
            .expect("expected the closure punctuation diagnostic");
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diagnostic.source.as_deref(), Some("glyph"));
        assert_eq!(diagnostic.range.start.line, 1);
        assert!(diagnostic.range.end.character > diagnostic.range.start.character);
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for GlyphLanguageServer {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "glyphlsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "glyphlsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let doc = DocumentState::new(params.text_document.text, params.text_document.version);
        self.documents.insert(uri.clone(), doc);
        self.compile_and_publish(uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        if let Some(change) = params.content_changes.into_iter().last() {
            let doc = DocumentState::new(change.text, params.text_document.version);
            self.documents.insert(uri.clone(), doc);
        }
        self.debounced_compile(uri).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        if let Some(text) = params.text {
            let version = self.documents.get(&uri).map(|d| d.version).unwrap_or(0);
            let doc = DocumentState::new(text, version);
            self.documents.insert(uri.clone(), doc);
        }
        self.compile_and_publish(uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.documents.remove(&uri);
        self.client.publish_diagnostics(uri, vec![], None).await;
    }
}
