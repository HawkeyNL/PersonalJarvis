//! Portfolio domain: manually-entered holdings (no market-data provider yet).
//!
//! Quantities and money use `Decimal` — never floats (blueprint principle).

// `sqlx::Error` is a large third-party error we surface via `PortfolioError`.
#![allow(clippy::result_large_err)]

use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// SurrealDB repository under parity construction. Exact amounts remain
/// decimal strings in the datastore; no floating-point portfolio values.
pub mod surreal;

/// Errors returned by the portfolio repository.
#[derive(Debug, thiserror::Error)]
pub enum PortfolioError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("database error")]
    DatabaseSurreal,
    #[error("invalid stored decimal")]
    InvalidDecimal,
}

/// A single manually-entered holding.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
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
    /// Cost basis = quantity * average cost.
    pub fn cost_basis(&self) -> Decimal {
        self.quantity * self.avg_cost
    }
}

/// Add a holding for a user.
pub async fn add_holding(
    pool: &PgPool,
    user_id: Uuid,
    symbol: &str,
    quantity: Decimal,
    avg_cost: Decimal,
    currency: &str,
) -> Result<Holding, PortfolioError> {
    let holding = sqlx::query_as::<_, Holding>(
        "insert into holdings (id, user_id, symbol, quantity, avg_cost, currency) \
         values ($1, $2, $3, $4, $5, $6) returning *",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(symbol)
    .bind(quantity)
    .bind(avg_cost)
    .bind(currency)
    .fetch_one(pool)
    .await?;
    Ok(holding)
}

/// List a user's holdings, alphabetically by symbol.
pub async fn list_holdings(pool: &PgPool, user_id: Uuid) -> Result<Vec<Holding>, PortfolioError> {
    let holdings = sqlx::query_as::<_, Holding>(
        "select * from holdings where user_id = $1 order by symbol asc",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(holdings)
}

/// Delete a holding owned by the user. Returns true if a row was removed.
pub async fn delete_holding(
    pool: &PgPool,
    user_id: Uuid,
    id: Uuid,
) -> Result<bool, PortfolioError> {
    let result = sqlx::query("delete from holdings where id = $1 and user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn add_list_delete(pool: PgPool) -> Result<(), PortfolioError> {
        // A holding needs a user; insert one directly.
        let user_id = Uuid::now_v7();
        sqlx::query("insert into users (id, display_name) values ($1, $2)")
            .bind(user_id)
            .bind("Tester")
            .execute(&pool)
            .await?;

        let holding = add_holding(&pool, user_id, "AAPL", dec("10"), dec("150.25"), "USD").await?;
        assert_eq!(holding.symbol, "AAPL");
        assert_eq!(holding.cost_basis().normalize(), dec("1502.50").normalize());

        let list = list_holdings(&pool, user_id).await?;
        assert_eq!(list.len(), 1);

        assert!(delete_holding(&pool, user_id, holding.id).await?);
        assert!(list_holdings(&pool, user_id).await?.is_empty());
        // Deleting again is a no-op.
        assert!(!delete_holding(&pool, user_id, holding.id).await?);
        Ok(())
    }
}
