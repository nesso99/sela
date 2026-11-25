//! M3-2 SQL autocomplete: wire [`sela_core::complete`] into the GPUI Editor
//! via [`CompletionProvider`], and keep a schema snapshot on the workspace.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::component::input::{
    CompletionMenuOptions, CompletionProvider, EditorState, Enter, Rope, RopeExt,
};
use gpui_kit::{px, App, Context, Entity, Task, Window};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    CompletionTriggerKind, TextEdit,
};

use sela_core::{
    complete, is_catalog_refresh_statement, should_auto_trigger, CatalogStatus, ColumnMeta,
    CompletionKind, DatabaseSession, Dialect, SchemaEntry, SchemaSnapshot, TableEntry,
};
use sela_drivers::drive;

use super::Workspace;

/// Shared catalog view for every SQL editor on this connection.
#[derive(Clone)]
pub(crate) struct SchemaCacheHandle {
    inner: Rc<RefCell<SchemaCacheState>>,
}

#[derive(Clone)]
struct SchemaCacheState {
    status: CatalogStatus,
    dialect: Dialect,
    /// Schema that scopes unqualified names (sidebar / USE / search_path).
    active_schema: Option<String>,
    generation: u64,
}

impl SchemaCacheHandle {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(SchemaCacheState {
                status: CatalogStatus::Empty,
                dialect: Dialect::Sqlite,
                active_schema: None,
                generation: 0,
            })),
        }
    }

    pub fn status(&self) -> CatalogStatus {
        self.inner.borrow().status.clone()
    }

    pub fn set_loading(&self, dialect: Dialect) {
        let mut g = self.inner.borrow_mut();
        g.dialect = dialect;
        g.status = CatalogStatus::Loading;
        g.generation = g.generation.wrapping_add(1);
    }

    pub fn set_snapshot(
        &self,
        dialect: Dialect,
        snapshot: SchemaSnapshot,
        active_schema: Option<String>,
    ) {
        let mut g = self.inner.borrow_mut();
        g.dialect = dialect;
        g.active_schema = active_schema;
        g.status = if snapshot.schemas.is_empty()
            || snapshot.schemas.iter().all(|s| s.tables.is_empty())
        {
            CatalogStatus::Empty
        } else {
            CatalogStatus::Ready(snapshot)
        };
        g.generation = g.generation.wrapping_add(1);
    }

    pub fn set_empty(&self, dialect: Dialect) {
        let mut g = self.inner.borrow_mut();
        g.dialect = dialect;
        g.status = CatalogStatus::Empty;
        g.generation = g.generation.wrapping_add(1);
    }

    pub fn set_active_schema(&self, schema: Option<String>) {
        let mut g = self.inner.borrow_mut();
        if g.active_schema != schema {
            g.active_schema = schema;
            g.generation = g.generation.wrapping_add(1);
        }
    }

    pub fn dialect(&self) -> Dialect {
        self.inner.borrow().dialect
    }

    pub fn active_schema(&self) -> Option<String> {
        self.inner.borrow().active_schema.clone()
    }
}

/// [`CompletionProvider`] backed by the workspace schema cache.
pub(super) struct SqlCompletionProvider {
    cache: SchemaCacheHandle,
}

impl SqlCompletionProvider {
    pub fn new(cache: SchemaCacheHandle) -> Self {
        Self { cache }
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: lsp_types::CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let buffer = text.to_string();
        let offset = offset.min(buffer.len());
        let dialect = self.cache.dialect();
        let status = self.cache.status();
        let active = self.cache.active_schema();
        let manual = trigger.trigger_kind == CompletionTriggerKind::INVOKED;
        if !manual && !should_auto_trigger(dialect, &buffer, offset) {
            return Task::ready(Ok(CompletionResponse::Array(vec![])));
        }
        let items = complete(dialect, &status, &buffer, offset, active.as_deref());
        let lsp_items = items
            .into_iter()
            .map(|item| to_lsp_item(text, offset, item))
            .collect::<Vec<_>>();
        Task::ready(Ok(CompletionResponse::Array(lsp_items)))
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        let dialect = self.cache.dialect();
        if new_text.contains('.') {
            return true;
        }
        new_text.chars().any(|ch| is_trigger_ident(dialect, ch))
    }
}

fn is_trigger_ident(dialect: Dialect, ch: char) -> bool {
    ch.is_alphabetic()
        || matches!(ch, '0'..='9' | '_')
        || (matches!(dialect, Dialect::Mysql) && ch == '$')
}

fn to_lsp_item(text: &Rope, caret: usize, item: sela_core::CompletionSuggestion) -> CompletionItem {
    let replace_start = caret.saturating_sub(item.replace_len);
    let start = text.offset_to_position(replace_start);
    let end = text.offset_to_position(caret);
    let kind = match item.kind {
        CompletionKind::Keyword => CompletionItemKind::KEYWORD,
        CompletionKind::Schema => CompletionItemKind::MODULE,
        CompletionKind::Table => CompletionItemKind::CLASS,
        CompletionKind::Column => CompletionItemKind::FIELD,
        CompletionKind::Alias => CompletionItemKind::VARIABLE,
        CompletionKind::Status => CompletionItemKind::TEXT,
    };
    CompletionItem {
        label: item.label,
        detail: if item.detail.is_empty() {
            None
        } else {
            Some(item.detail)
        },
        kind: Some(kind),
        // Status rows must not change the buffer (AU-16): insert is empty.
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range: lsp_types::Range { start, end },
            new_text: item.insert,
        })),
        ..Default::default()
    }
}

impl Workspace {
    pub(super) fn install_sql_completions(&self, editor: &Entity<EditorState>, cx: &mut App) {
        let cache = self.schema_cache.clone();
        let provider = Rc::new(SqlCompletionProvider::new(cache));
        editor.update(cx, |state, _cx| {
            state.lsp_mut().completion_provider = Some(provider);
            state.lsp_mut().completion_menu = CompletionMenuOptions {
                max_width: px(360.),
            };
        });
    }

    /// Refresh schema cache from the live session (connect / Refresh schema / DDL).
    pub(super) fn reload_schema_cache(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            self.schema_cache.set_empty(Dialect::Sqlite);
            return;
        };
        let dialect = session.dialect();
        self.schema_cache.set_loading(dialect);
        let active_schema = self
            .schema_cache
            .active_schema()
            .or_else(|| self.catalog.first().map(|s| s.name.clone()));
        let cache = self.schema_cache.clone();
        // Do not mark the tab busy — catalog refresh for autocomplete must not
        // look like a user query (and must not log SQL text).
        cx.spawn(async move |this, async_cx| {
            let result = drive(async move { load_schema_snapshot(session.as_ref()).await }).await;
            this.update(async_cx, |this, cx| {
                match result {
                    Ok(snapshot) => {
                        let active = active_schema
                            .or_else(|| snapshot.schemas.first().map(|s| s.name.clone()));
                        cache.set_snapshot(dialect, snapshot, active);
                    }
                    Err(_) => {
                        cache.set_empty(dialect);
                    }
                }
                let _ = this;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn maybe_refresh_schema_after_sql(&mut self, sql: &str, cx: &mut Context<Self>) {
        // `refresh_catalog` also reloads the autocomplete schema cache on success.
        if is_catalog_refresh_statement(sql) {
            self.refresh_catalog(cx);
        }
    }

    pub(super) fn accept_completion_or_indent(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.active_view() else {
            return;
        };
        let open = view.sql.read(cx).completion_menu_state().open;
        if open {
            view.sql.update(cx, |state, cx| {
                let _ = state.route_overlay_action(
                    Box::new(Enter {
                        secondary: false,
                        shift: false,
                    }),
                    window,
                    cx,
                );
            });
        } else {
            // Tab while closed inserts indent (AU-13); kit IndentInline is
            // overridden by this binding so we insert a tab character.
            view.sql.update(cx, |state, cx| {
                state.insert("\t", window, cx);
            });
        }
    }

    pub(super) fn show_sql_completions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.active_view() else {
            return;
        };
        if !view.editor_focused(window, cx) {
            return;
        }
        view.sql.update(cx, |state, cx| {
            state.show_completions(window, cx);
        });
    }
}

/// Load schemas, tables, and columns for autocomplete (no query text logged).
pub(super) async fn load_schema_snapshot(
    session: &dyn DatabaseSession,
) -> Result<SchemaSnapshot, sela_core::DbError> {
    let schemas = session.list_schemas().await?;
    let mut entries = Vec::with_capacity(schemas.len());
    for schema in schemas {
        let tables = session.list_tables(&schema.name).await?;
        let mut table_entries = Vec::with_capacity(tables.len());
        for table in tables {
            let columns = match session.describe_table(&schema.name, &table.name).await {
                Ok(structure) => structure
                    .columns
                    .into_iter()
                    .map(|c| ColumnMeta {
                        name: c.name,
                        type_name: c.type_name,
                    })
                    .collect(),
                Err(_) => Vec::new(),
            };
            table_entries.push(TableEntry {
                name: table.name,
                columns,
            });
        }
        entries.push(SchemaEntry {
            name: schema.name,
            tables: table_entries,
        });
    }
    Ok(SchemaSnapshot { schemas: entries })
}

/// Test helper: attach a ready cache + provider to an editor.
#[cfg(test)]
pub(crate) fn install_test_completions(
    editor: &Entity<EditorState>,
    cache: SchemaCacheHandle,
    cx: &mut App,
) {
    let provider = Rc::new(SqlCompletionProvider::new(cache));
    editor.update(cx, |state, _cx| {
        state.lsp_mut().completion_provider = Some(provider);
        state.lsp_mut().completion_menu = CompletionMenuOptions {
            max_width: px(360.),
        };
    });
}
