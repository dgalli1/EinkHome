//! The long-press context menu (C eh_draw_context): a centred white sheet
//! with the action rows.  Geometry matches the harness's context_geom
//! (sheet centred on the FULL screen; title band 72 + n*96 + 24 rows).

/// A long-press action row (C eh_context_action).
#[derive(Clone, Copy, PartialEq)]
pub enum ContextAction {
    Open,
    Details,
    Download,
    Delete,
    DownloadAll,
    DeleteAll,
}

impl ContextAction {
    /// The i18n key of the row's label (C eh_context labels).
    pub fn label_key(self) -> &'static str {
        match self {
            ContextAction::Open => "ctx.open",
            ContextAction::Details => "ctx.details",
            ContextAction::Download => "ctx.download",
            ContextAction::Delete => "ctx.delete",
            ContextAction::DownloadAll => "ctx.download_all",
            ContextAction::DeleteAll => "ctx.delete_series",
        }
    }
}
