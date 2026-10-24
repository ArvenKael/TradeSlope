pub mod trade;
pub mod welcome;

pub type PageId = usize;

pub struct WorkspacePage {
    pub id: PageId,
    pub name: String,
    pub symbol: String,
    pub cards: Vec<CardKind>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    Chart,
    Watchlist,
    Orders,
    Positions,
}

impl CardKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Chart => "Chart",
            Self::Watchlist => "Watchlist",
            Self::Orders => "Orders",
            Self::Positions => "Positions",
        }
    }
}
