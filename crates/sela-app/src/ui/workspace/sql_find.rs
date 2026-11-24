//! In-editor find (header bar).

use gpui_kit::Context;

use super::Workspace;

impl Workspace {
    pub(super) fn toggle_sql_find(
        &mut self,
        window: &mut gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.find_open = !view.find_open;
            if view.find_open {
                view.find_input
                    .update(cx, |state, cx| state.focus(window, cx));
            }
        }
        cx.notify();
    }

    pub(super) fn close_sql_find(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.find_open = false;
        }
        cx.notify();
    }

    pub(super) fn refresh_find_matches(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        let (buffer, query) = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            (
                view.sql.read(cx).value().to_string(),
                view.find_input.read(cx).value().to_string(),
            )
        };
        let matches = if query.is_empty() {
            Vec::new()
        } else {
            find_all_matches(&buffer, &query)
        };
        if let Some(view) = self.view_mut(tab_id) {
            view.find_matches = matches;
            view.find_match_ix = 0;
        }
    }

    pub(super) fn find_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active().map(|t| t.id) else {
            return;
        };
        self.refresh_find_matches(cx);
        let (matches, sql_entity) = {
            let Some(view) = self.views.get(&tab_id) else {
                return;
            };
            (view.find_matches.clone(), view.sql.clone())
        };
        if matches.is_empty() {
            return;
        }
        let ix = {
            let Some(view) = self.view_mut(tab_id) else {
                return;
            };
            let len = matches.len();
            if delta >= 0 {
                view.find_match_ix = (view.find_match_ix + delta as usize) % len;
            } else {
                let back = delta.unsigned_abs();
                view.find_match_ix = view
                    .find_match_ix
                    .checked_sub(back)
                    .unwrap_or(len - back.min(len));
            }
            view.find_match_ix
        };
        let range = matches[ix].clone();
        sql_entity.update(cx, |state, cx| state.set_selected_range(range, cx));
        cx.notify();
    }
}

fn find_all_matches(haystack: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    let lower_hay = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let mut matches = Vec::new();
    let mut search_at = 0;
    while let Some(rel) = lower_hay[search_at..].find(&lower_needle) {
        let start = search_at + rel;
        let end = start + needle.len().min(haystack.len().saturating_sub(start));
        matches.push(start..end);
        search_at = start + lower_needle.len().max(1);
        if search_at >= lower_hay.len() {
            break;
        }
    }
    matches
}
