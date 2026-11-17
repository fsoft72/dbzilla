//! Tab model of a workspace and the rules for opening objects (SPEC 7.6).

use dbzilla_core::model::TableRef;

/// Identifier of a tab, unique inside a workspace.
pub type TabId = u64;

/// What a tab shows, as far as opening rules are concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabTarget {
    Table(TableRef),
    Sql(TabId),
    /// The ER diagram (one per workspace).
    Diagram,
    /// The metrics board (one per workspace).
    Metrics,
}

/// The facts about an existing tab that the opening rules need.
#[derive(Debug, Clone)]
pub struct TabMeta {
    pub id: TabId,
    pub target: TabTarget,
    pub pinned: bool,
    pub dirty: bool,
}

/// How the user asked to open something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Single click: reuse a clean preview tab.
    Preview,
    /// Double click: like preview, but the tab is kept.
    Pin,
    /// "Open in new tab": always a new tab.
    NewTab,
}

/// What to do with the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenDecision {
    Focus(TabId),
    Replace(TabId),
    New,
}

/// True when the tab can be replaced by another object (clean, unpinned, table-like).
fn replaceable(t: &TabMeta) -> bool {
    !t.pinned && !t.dirty && matches!(t.target, TabTarget::Table(_))
}

/// Decides how to open `target`: focus an existing tab, replace a clean preview tab or open a new one.
pub fn decide_open(tabs: &[TabMeta], focused: Option<TabId>, target: &TabTarget, mode: OpenMode) -> OpenDecision {
    if mode == OpenMode::NewTab {
        return OpenDecision::New;
    }
    if let Some(t) = tabs.iter().find(|t| &t.target == target) {
        return OpenDecision::Focus(t.id);
    }
    if let Some(f) = focused.and_then(|f| tabs.iter().find(|t| t.id == f)) {
        if replaceable(f) {
            return OpenDecision::Replace(f.id);
        }
    }
    // the focused tab is busy (SQL editor, pending changes, pinned): reuse another clean preview tab
    match tabs.iter().find(|t| replaceable(t)) {
        Some(t) => OpenDecision::Replace(t.id),
        None => OpenDecision::New,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(id: TabId, name: &str, pinned: bool, dirty: bool) -> TabMeta {
        TabMeta { id, target: TabTarget::Table(TableRef::simple(name)), pinned, dirty }
    }

    fn target(name: &str) -> TabTarget {
        TabTarget::Table(TableRef::simple(name))
    }

    /// With no tabs, a new one is opened.
    #[test]
    fn empty_workspace_opens_new() {
        assert_eq!(decide_open(&[], None, &target("a"), OpenMode::Preview), OpenDecision::New);
    }

    /// A clean, unpinned focused table tab is replaced (preview behaviour).
    #[test]
    fn clean_preview_tab_is_replaced() {
        let tabs = [table(1, "a", false, false)];
        assert_eq!(decide_open(&tabs, Some(1), &target("b"), OpenMode::Preview), OpenDecision::Replace(1));
        assert_eq!(decide_open(&tabs, Some(1), &target("b"), OpenMode::Pin), OpenDecision::Replace(1));
    }

    /// Pinned or dirty tabs are kept, so a new tab opens.
    #[test]
    fn pinned_or_dirty_tabs_are_kept() {
        assert_eq!(decide_open(&[table(1, "a", true, false)], Some(1), &target("b"), OpenMode::Preview), OpenDecision::New);
        assert_eq!(decide_open(&[table(1, "a", false, true)], Some(1), &target("b"), OpenMode::Preview), OpenDecision::New);
    }

    /// An SQL editor is never replaced; another clean table tab is reused if there is one.
    #[test]
    fn sql_editor_is_kept_and_other_preview_reused() {
        let sql = TabMeta { id: 1, target: TabTarget::Sql(1), pinned: false, dirty: false };
        assert_eq!(decide_open(std::slice::from_ref(&sql), Some(1), &target("b"), OpenMode::Preview), OpenDecision::New);
        let tabs = [sql, table(2, "a", false, false)];
        assert_eq!(decide_open(&tabs, Some(1), &target("b"), OpenMode::Preview), OpenDecision::Replace(2));
    }

    /// An already open object is focused; "new tab" always opens one.
    #[test]
    fn existing_is_focused_and_new_tab_forces_new() {
        let tabs = [table(1, "a", false, false), table(2, "b", true, false)];
        assert_eq!(decide_open(&tabs, Some(1), &target("b"), OpenMode::Preview), OpenDecision::Focus(2));
        assert_eq!(decide_open(&tabs, Some(1), &target("b"), OpenMode::NewTab), OpenDecision::New);
    }
}
