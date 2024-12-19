use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use askama::Template;
use serde::{Deserialize, Serialize};
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};
use tree_sitter::{Language, Parser, Query, QueryCursor};

use regex::Regex;

// Advanced dbt project configuration parsing
#[derive(Debug, Deserialize, Serialize)]
struct DbtProjectConfig {
    name: String,
    version: String,
    profile: String,
    target_path: Option<PathBuf>,
    models: HashMap<String, ModelConfig>,
    seeds: Option<HashMap<String, SeedConfig>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ModelConfig {
    materialized: Option<String>,
    tags: Option<Vec<String>>,
    schema: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SeedConfig {
    quote_columns: Option<bool>,
}

// Enhanced model representation
#[derive(Debug, Clone)]
struct DbtModel {
    name: String,
    path: PathBuf,
    model_type: ModelType,
    refs: Vec<String>,
    sources: Vec<String>,
    tags: Vec<String>,
}

#[derive(Debug, Clone)]
enum ModelType {
    View,
    Table,
    Incremental,
    Seed,
    Snapshot,
}

// Advanced LSP state management
struct DbtLanguageServerState {
    project_root: PathBuf,
    models: HashMap<PathBuf, DbtModel>,
    macros: HashMap<String, MacroDefinition>,
    project_config: Option<DbtProjectConfig>,
}

#[derive(Debug, Clone)]
struct MacroDefinition {
    name: String,
    file_path: PathBuf,
    arguments: Vec<String>,
}

// Main Language Server struct
struct DbtLanguageServer {
    client: Client,
    state: Arc<Mutex<DbtLanguageServerState>>,
}

impl DbtLanguageServer {
    fn new(client: Client) -> Self {
        Self {
            client,
            state: Arc::new(Mutex::new(DbtLanguageServerState {
                project_root: std::env::current_dir().unwrap(),
                models: HashMap::new(),
                macros: HashMap::new(),
                project_config: None,
            })),
        }
    }

    // Advanced project parsing with deep introspection
    fn parse_dbt_project(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        
        // Find dbt_project.yml
        let project_file = find_dbt_project_file(&state.project_root)?;
        
        // Parse project configuration
        let project_config: DbtProjectConfig = serde_yaml::from_str(&std::fs::read_to_string(&project_file)?)?;
        state.project_config = Some(project_config);

        // Parse models directory
        let models_dir = state.project_root.join("models");
        for entry in walkdir::WalkDir::new(models_dir) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let path = entry.path().to_path_buf();
                if path.extension().map_or(false, |ext| ext == "sql") {
                    let model = parse_dbt_model(&path)?;
                    state.models.insert(path, model);
                }
            }
        }

        // Parse macros directory
        let macros_dir = state.project_root.join("macros");
        for entry in walkdir::WalkDir::new(macros_dir) {
            let entry = entry?;
            if entry.file_type().is_file() {
                let path = entry.path().to_path_buf();
                if path.extension().map_or(false, |ext| ext == "sql") {
                    let macros = parse_dbt_macros(&path)?;
                    for macro_def in macros {
                        state.macros.insert(macro_def.name.clone(), macro_def);
                    }
                }
            }
        }

        Ok(())
    }

    // Advanced SQL and Jinja parsing
    fn analyze_dbt_model(&self, uri: &Url) -> LspResult<Vec<Diagnostic>> {
        let file_path = uri.to_file_path().map_err(|_| tower_lsp::jsonrpc::Error::invalid_params("Invalid file path"))?;
        
        let sql_content = std::fs::read_to_string(&file_path)
            .context("Could not read SQL file")
            .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        let mut diagnostics = Vec::new();

        // Advanced SQL parsing with tree-sitter
        let mut parser = Parser::new();
        parser.set_language(tree_sitter_sql::language())?;
        
        let tree = parser.parse(&sql_content, None).ok_or_else(|| 
            tower_lsp::jsonrpc::Error::invalid_params("Failed to parse SQL")
        )?;

        // Example: Find potential SQL syntax issues
        let query = Query::new(
            tree.language(),
            "(ERROR) @error"
        )?;

        let mut query_cursor = QueryCursor::new();
        for m in query_cursor.matches(&query, tree.root_node(), sql_content.as_bytes()) {
            for capture in m.captures {
                let range = capture.node.range();
                diagnostics.push(Diagnostic {
                    range: Range {
                        start: Position::new(range.start_point.row as u32, range.start_point.column as u32),
                        end: Position::new(range.end_point.row as u32, range.end_point.column as u32),
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    message: "Potential SQL syntax error".to_string(),
                    ..Default::default()
                });
            }
        }

        Ok(diagnostics)
    }

    // Advanced code completion
    fn provide_completion(&self, context: CompletionContext) -> LspResult<Vec<CompletionItem>> {
        let state = self.state.lock().unwrap();
        let mut completions = Vec::new();

        // dbt-specific completions
        completions.extend(vec![
            CompletionItem::new_simple("ref".to_string(), "dbt reference to another model".to_string()),
            CompletionItem::new_simple("source".to_string(), "dbt source table reference".to_string()),
            CompletionItem::new_simple("config".to_string(), "dbt model configuration block".to_string()),
        ]);

        // Macro completions
        for (name, macro_def) in &state.macros {
            completions.push(CompletionItem {
                label: name.clone(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(format!("Macro with {} arguments", macro_def.arguments.len())),
                ..Default::default()
            });
        }

        // Model name completions
        for model in state.models.values() {
            completions.push(CompletionItem {
                label: model.name.clone(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(format!("{:?} model", model.model_type)),
                ..Default::default()
            });
        }

        Ok(completions)
    }
}

// Utility functions for parsing
fn find_dbt_project_file(root: &Path) -> Result<PathBuf> {
    for entry in walkdir::WalkDir::new(root).max_depth(2) {
        let entry = entry?;
        if entry.file_name() == "dbt_project.yml" {
            return Ok(entry.path().to_path_buf());
        }
    }
    Err(anyhow::anyhow!("No dbt_project.yml found"))
}

fn parse_dbt_model(path: &Path) -> Result<DbtModel> {
    let content = std::fs::read_to_string(path)?;
    
    // Extract refs and sources using regex
    let refs: Vec<String> = regex::Regex::new(r###"\{\{\s*ref\(['"]([^'\"]+)['\"]\)\s*\}\}"###)
        .unwrap()
        .captures_iter(&content)
        .map(|cap| cap[1].to_string())
        .collect();

    let sources: Vec<String> = regex::Regex::new(r###"\{\{\s*source\(['"]([^'\"]+)['\"]\)\s*\}\}"###)
        .unwrap()
        .captures_iter(&content)
        .map(|cap| cap[1].to_string())
        .collect();

    Ok(DbtModel {
        name: path.file_stem().unwrap().to_str().unwrap().to_string(),
        path: path.to_path_buf(),
        model_type: ModelType::View,  // Enhanced type detection would go here
        refs,
        sources,
        tags: Vec::new(),
    })
}

fn parse_dbt_macros(path: &Path) -> Result<Vec<MacroDefinition>> {
    let content = std::fs::read_to_string(path)?;
    
    // Basic macro parsing with regex
    let macro_regex = regex::Regex::new(r"{% macro\s+(\w+)\((.*?)\)\s*%}").unwrap();
    
    macro_regex.captures_iter(&content)
        .map(|cap| {
            Ok(MacroDefinition {
                name: cap[1].to_string(),
                file_path: path.to_path_buf(),
                arguments: cap[2]
                    .split(',')
                    .map(|arg| arg.trim().to_string())
                    .filter(|arg| !arg.is_empty())
                    .collect(),
            })
        })
        .collect()
}

// Implement full LSP traits
#[tower_lsp::async_trait]
impl LanguageServer for DbtLanguageServer {
    async fn initialize(&self, _params: InitializeParams) -> LspResult<InitializeResult> {
        // Full project parsing on initialization
        self.parse_dbt_project().map_err(|_| 
            tower_lsp::jsonrpc::Error::invalid_params("Failed to parse dbt project")
        )?;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                completion_provider: Some(CompletionOptions::default()),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client.log_message(MessageType::INFO, "DBT Language Server fully initialized").await;
    }

    async fn completion(&self, _params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let completions = self.provide_completion(CompletionContext::default())?;
        Ok(Some(CompletionResponse::List(CompletionList {
            is_incomplete: false,
            items: completions,
        })))
    }

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        // Placeholder hover implementation
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: "Hover information for dbt model".to_string(),
            }),
            range: None,
        }))
    }

    async fn goto_definition(&self, params: GotoDefinitionParams) -> LspResult<Option<GotoDefinitionResponse>> {
        // Placeholder definition lookup
        Ok(None)
    }

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        // Placeholder reference lookup
        Ok(None)
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| DbtLanguageServer::new(client));
    Server::new(stdin, stdout, socket).serve(service).await;
}
