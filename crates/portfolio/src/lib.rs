//! Portfolio domain: manually-entered holdings (no market-data provider yet).
//!
//! Quantities and money use `Decimal` — never floats.

use rust_decimal::Decimal;
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

/// The SurrealDB portfolio repository. Decimal values remain strings at the
/// datastore boundary and are parsed before any arithmetic.
pub mod surreal;

#[derive(Debug, thiserror::Error)]
pub enum PortfolioError {
    #[error("database error")]
    DatabaseSurreal,
    #[error("invalid stored decimal")]
    InvalidDecimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct Holding {
    pub id: Uuid,
    pub user_id: Uuid,
    pub symbol: String,
    pub quantity: Decimal,
    pub avg_cost: Decimal,
    pub currency: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl Holding {
    pub fn cost_basis(&self) -> Decimal {
        self.quantity * self.avg_cost
    }
}

pub use surreal::{add_holding, delete_holding, list_holdings};
