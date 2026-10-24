pub mod builder;
pub mod welcome;

pub mod drag;
pub mod workspace;

pub type PageId = usize;

pub struct WorkspacePage {
    pub id: PageId,
    pub name: String,
    pub workspace: workspace::Workspace,
}

#[derive(Clone, PartialEq, Eq)]
pub enum CardKind {
    Native(String),
    JavaScript(String),
}

impl CardKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Native(_) => "Native card",
            Self::JavaScript(_) => "JavaScript card",
        }
    }

    pub fn extension_id(&self) -> Option<&str> {
        match self {
            Self::Native(id) | Self::JavaScript(id) => Some(id),
        }
    }
}
