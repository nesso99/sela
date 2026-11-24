//! SQL editor gutter and error underline decorations.

use gpui_kit::component::input::{GutterDecoration, TextDecoration};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{px, Context, HighlightStyle, UnderlineStyle};

use sela_core::{
    line_col_to_byte_range, parse_postgres_error_position, split_statements, statement_index_at,
    SqlError,
};

use super::Workspace;

impl Workspace {
    pub(super) fn sync_active_sql_decorations(&self, cx: &mut Context<Self>) {
        let Some(view) = self.active_view() else {
            return;
        };
        let buffer = view.sql.read(cx).value().to_string();
        let selection = view.sql.read(cx).selected_range();
        let caret = selection.end;
        let has_selection = !selection.is_empty();
        let statements = split_statements(&buffer, self.active_dialect()).ok();
        let muted = cx.theme().muted_foreground;
        let danger = cx.theme().danger;
        let gutter = view.sql_gutter.clone();
        let marks = view.sql_error_marks.clone();

        let mut gutter_decorations = Vec::new();
        let mut text_decorations = Vec::new();

        if !has_selection {
            if let Some(stmts) = statements.as_ref() {
                let ix = view
                    .sql_running_statement_index
                    .or_else(|| statement_index_at(stmts, caret));
                if let Some(i) = ix {
                    if let Some(stmt) = stmts.get(i) {
                        gutter_decorations
                            .push(GutterDecoration::new(stmt.range.clone()).with_color(muted));
                    }
                }
            }
        }

        if let Some(SqlError::UnclosedQuote { offset }) = &view.local_sql_error {
            let end = (*offset + 1).min(buffer.len());
            gutter_decorations.push(GutterDecoration::new(*offset..end).with_color(danger));
        } else if let Some(err_msg) = &view.error {
            if let Some((line, col)) = parse_postgres_error_position(err_msg) {
                if let Some(byte_range) = line_col_to_byte_range(&buffer, line, col) {
                    if col.is_some() {
                        let style = HighlightStyle {
                            underline: Some(UnderlineStyle {
                                thickness: px(1.),
                                color: Some(danger),
                                wavy: false,
                            }),
                            color: Some(danger),
                            ..Default::default()
                        };
                        text_decorations.push(TextDecoration::new(byte_range, style));
                    } else {
                        gutter_decorations
                            .push(GutterDecoration::new(byte_range).with_color(danger));
                    }
                }
            }
        }

        gutter.set(gutter_decorations, cx);
        marks.set(text_decorations, cx);
    }

    pub(super) fn clear_sql_error_decorations(
        &mut self,
        tab_id: sela_core::TabId,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.views.get(&tab_id) else {
            return;
        };
        view.sql_error_marks.clear(cx);
        view.sql_gutter.clear(cx);
    }
}
