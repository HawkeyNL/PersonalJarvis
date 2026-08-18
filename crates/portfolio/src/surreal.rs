use std::str::FromStr;

use serde::de::DeserializeOwned;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use jarvis_store::Database;

use super::{Holding, PortfolioError};

#[derive(serde::Deserialize)]
struct StoredHolding {
    #[serde(with = "uuid::serde::hyphenated")]
    id: Uuid,
    #[serde(with = "uuid::serde::hyphenated")]
    user_id: Uuid,
    symbol: String,
    quantity: String,
    avg_cost: String,
    currency: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

impl TryFrom<StoredHolding> for Holding {
    type Error = PortfolioError;

    fn try_from(value: StoredHolding) -> Result<Self, Self::Error> {
        Ok(Self {
            id: value.id,
            user_id: value.user_id,
            symbol: value.symbol,
            quantity: rust_decimal::Decimal::from_str(&value.quantity)
                .map_err(|_| PortfolioError::InvalidDecimal)?,
            avg_cost: rust_decimal::Decimal::from_str(&value.avg_cost)
                .map_err(|_| PortfolioError::InvalidDecimal)?,
            currency: value.currency,
            created_at: value.created_at,
            updated_at: value.updated_at,
        })
    }
}

async fn one<T: DeserializeOwned>(
    db: &Database,
    query: &str,
    bindings: serde_json::Value,
) -> Result<Option<T>, PortfolioError> {
    let mut response = db.query(query).bind(bindings).await.map_err(|error| {
        tracing::warn!(%error, "failed to query SurrealDB holding");
        PortfolioError::DatabaseSurreal
    })?;
    response.take(0).map_err(|error| {
        tracing::warn!(%error, "failed to decode SurrealDB holding");
        PortfolioError::DatabaseSurreal
    })
}

const FIELDS: &str =
    "record::id(id) AS id, user_id, symbol, quantity, avg_cost, currency, created_at, updated_at";

pub async fn add_holding(
    db: &Database,
    user_id: Uuid,
    symbol: &str,
    quantity: rust_decimal::Decimal,
    avg_cost: rust_decimal::Decimal,
    currency: &str,
) -> Result<Holding, PortfolioError> {
    let id = Uuid::now_v7();
    let response = db.query(
        "CREATE holdings SET id = $id, user_id = $user_id, symbol = $symbol, quantity = $quantity, \
         avg_cost = $avg_cost, currency = $currency, created_at = time::now(), updated_at = time::now() RETURN NONE",
    ).bind(json!({
        "id": id.to_string(), "user_id": user_id.to_string(), "symbol": symbol,
        "quantity": quantity.to_string(), "avg_cost": avg_cost.to_string(), "currency": currency,
    })).await.map_err(|error| {
        tracing::warn!(%error, "failed to create SurrealDB holding");
        PortfolioError::DatabaseSurreal
    })?;
    response.check().map_err(|error| {
        tracing::warn!(%error, "failed to validate SurrealDB holding create");
        PortfolioError::DatabaseSurreal
    })?;
    let row: StoredHolding = one(db, &format!("SELECT {FIELDS} FROM holdings WHERE record::id(id) = $id AND user_id = $user_id LIMIT 1"), json!({"id": id.to_string(), "user_id": user_id.to_string()})).await?.ok_or(PortfolioError::DatabaseSurreal)?;
    row.try_into()
}

pub async fn list_holdings(db: &Database, user_id: Uuid) -> Result<Vec<Holding>, PortfolioError> {
    let mut response = db
        .query(format!(
            "SELECT {FIELDS} FROM holdings WHERE user_id = $user_id ORDER BY symbol"
        ))
        .bind(json!({"user_id": user_id.to_string()}))
        .await
        .map_err(|_| PortfolioError::DatabaseSurreal)?;
    let rows: Vec<StoredHolding> = response
        .take(0)
        .map_err(|_| PortfolioError::DatabaseSurreal)?;
    rows.into_iter().map(TryInto::try_into).collect()
}

pub async fn delete_holding(
    db: &Database,
    user_id: Uuid,
    id: Uuid,
) -> Result<bool, PortfolioError> {
    let bindings = json!({"id": id.to_string(), "user_id": user_id.to_string()});
    let exists: Option<StoredHolding> = one(
        db,
        &format!("SELECT {FIELDS} FROM holdings WHERE record::id(id) = $id AND user_id = $user_id LIMIT 1"),
        bindings.clone(),
    )
    .await?;
    if exists.is_none() {
        return Ok(false);
    }
    // The owner-scoped condition is repeated in the deletion itself. If a
    // concurrent delete wins, this remains a safe idempotent success.
    db.query("DELETE holdings WHERE record::id(id) = $id AND user_id = $user_id RETURN NONE")
        .bind(bindings)
        .await
        .map_err(|_| PortfolioError::DatabaseSurreal)?
        .check()
        .map_err(|_| PortfolioError::DatabaseSurreal)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::{env, str::FromStr};

    use rust_decimal::Decimal;
    use surrealdb::{engine::remote::ws::Ws, opt::auth::Root, Surreal};

    use super::*;

    #[tokio::test]
    #[ignore = "requires JARVIS_SURREAL_TEST_* and a disposable SurrealDB server"]
    async fn preserves_decimal_holdings_and_owner_scope() -> Result<(), Box<dyn std::error::Error>>
    {
        let endpoint = env::var("JARVIS_SURREAL_TEST_ENDPOINT")?;
        let username = env::var("JARVIS_SURREAL_TEST_USER")?;
        let password = env::var("JARVIS_SURREAL_TEST_PASS")?;
        let namespace = format!("jarvis_portfolio_{}", Uuid::now_v7().simple());
        let db = Surreal::new::<Ws>(&endpoint).await?;
        db.signin(Root {
            username: &username,
            password: &password,
        })
        .await?;
        db.use_ns(&namespace).use_db("core").await?;
        jarvis_store::apply_baseline_schema(&db).await?;

        let user_id = Uuid::now_v7();
        db.query("CREATE users SET id = $id, display_name = 'tester', status = 'active', created_at = time::now(), updated_at = time::now()")
            .bind(json!({"id": user_id.to_string()})).await?.check()?;
        let holding = add_holding(
            &db,
            user_id,
            "AAPL",
            Decimal::from_str("10.125")?,
            Decimal::from_str("150.25")?,
            "USD",
        )
        .await?;
        assert_eq!(holding.cost_basis(), Decimal::from_str("1521.28125")?);
        assert_eq!(list_holdings(&db, user_id).await?.len(), 1);
        assert!(!delete_holding(&db, Uuid::now_v7(), holding.id).await?);
        assert!(delete_holding(&db, user_id, holding.id).await?);
        Ok(())
    }
}
