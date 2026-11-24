//! SQL editor run modes: caret statement, selection, and run-all queue.

use std::ops::Range;

use gpui_kit::Context;

use sela_core::{split_statements, statement_index_at, SqlError};

use super::tab_view::{SqlResultSnapshot, MAX_RESULT_SNAPSHOTS};
use super::Workspace;

impl Workspace {
    pub(super) fn editor_field_focused(
        &self,
        window: &gpui_kit::Window,
        cx: &gpui_kit::App,
    ) -> bool {
        self.active_view()
            .map(|view| view.editor_focused(window, cx) || view.find_field_focused(window, cx))
            .unwrap_or(false)
    }

    pub(super) fn statements_for_run(&self, cx: &Context<Self>) -> Result<Vec<String>, SqlError> {
        let Some(view) = self.active_view() else {
            return Err(SqlError::Empty);
        };
        let buffer = view.sql.read(cx).value().to_string();
        if buffer.trim().is_empty() {
            return Err(SqlError::Empty);
        }
        let range = view.sql.read(cx).selected_range();
        if range.is_empty() {
            let statements = split_statements(&buffer)?;
            let ix = statement_index_at(&statements, range.end)
                .or(Some(0))
                .filter(|i| statements.get(*i).is_some())
                .ok_or(SqlError::Empty)?;
            Ok(vec![statements[ix].text.clone()])
        } else {
            let slice = substring_by_bytes(&buffer, &range);
            Ok(split_statements(slice)?
                .into_iter()
                .map(|s| s.text)
                .collect())
        }
    }

    pub(super) fn statements_for_run_all(
        &self,
        cx: &Context<Self>,
    ) -> Result<Vec<String>, SqlError> {
        let Some(view) = self.active_view() else {
            return Err(SqlError::Empty);
        };
        let buffer = view.sql.read(cx).value().to_string();
        if buffer.trim().is_empty() {
            return Err(SqlError::Empty);
        }
        Ok(split_statements(&buffer)?
            .into_iter()
            .map(|s| s.text)
            .collect())
    }

    pub(super) fn push_result_snapshot(&mut self, tab_id: sela_core::TabId, cx: &Context<Self>) {
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        let delegate = view.grid.read(cx).delegate();
        let statement_index = view.sql_run_statement_index.max(1);
        let failed = view.sql_failed_statement == Some(statement_index);
        let snapshot = SqlResultSnapshot {
            statement_index,
            result: delegate.result.clone(),
            page_offset: view.page_offset,
            page_len: view.page_len,
            total_rows: view.total_rows,
            row_count_kind: view.row_count_kind,
            paged: view.paged,
            truncated: view.truncated,
            can_load_more: view.can_load_more,
            note: view.note.clone(),
            status: view.status.clone(),
            failed,
            empty_message: delegate.empty_message.clone(),
            loading: delegate.loading,
        };
        if let Some(view) = self.view_mut(tab_id) {
            if view.result_snapshots.len() >= MAX_RESULT_SNAPSHOTS {
                view.result_snapshots.remove(0);
                view.note = Some("Showing the latest 8 results".into());
            }
            view.result_snapshots.push(snapshot);
            view.active_result_index = view.result_snapshots.len().saturating_sub(1);
        }
    }

    pub(super) fn show_result_snapshot(
        &mut self,
        tab_id: sela_core::TabId,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self
            .views
            .get(&tab_id)
            .and_then(|view| view.result_snapshots.get(index).cloned());
        let Some(snapshot) = snapshot else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.active_result_index = index;
            view.page_offset = snapshot.page_offset;
            view.page_len = snapshot.page_len;
            view.total_rows = snapshot.total_rows;
            view.row_count_kind = snapshot.row_count_kind;
            view.paged = snapshot.paged;
            view.truncated = snapshot.truncated;
            view.can_load_more = snapshot.can_load_more;
            view.note = snapshot.note.clone();
            view.status = snapshot.status.clone();
            view.grid.update(cx, |state, cx| {
                let delegate = state.delegate_mut();
                delegate.result = snapshot.result;
                delegate.loading = snapshot.loading;
                delegate.empty_message = snapshot.empty_message;
                state.refresh(cx);
            });
        }
        cx.notify();
    }

    pub(super) fn clear_run_queue(&mut self, tab_id: sela_core::TabId) {
        if let Some(view) = self.view_mut(tab_id) {
            view.sql_run_queue.clear();
            view.sql_running_statement_index = None;
        }
    }

    pub(super) fn format_query_failed_status(
        statement: Option<usize>,
        sqlstate: Option<&str>,
    ) -> String {
        match (statement, sqlstate) {
            (Some(n), Some(code)) => format!("Query failed · statement {n} · SQLSTATE {code}"),
            (Some(n), None) => format!("Query failed · statement {n}"),
            (_, Some(code)) => format!("Query failed · SQLSTATE {code}"),
            _ => "Query failed".into(),
        }
    }
}

fn substring_by_bytes(text: &str, range: &Range<usize>) -> &str {
    let start = range.start.min(text.len());
    let end = range.end.min(text.len());
    if start > end {
        return "";
    }
    &text[start..end]
}
