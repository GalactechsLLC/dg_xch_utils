use crate::StoreError;
use dg_xch_core::blockchain::{coin_record::CoinRecord, sized_bytes::Bytes32};
use dg_xch_core::protocols::wallet_sync::{WalletCoinRecordsRequest, WalletCoinRecordsResponse};
use sqlx::{Database, Encode, QueryBuilder, Type};
use std::collections::HashSet;

/// Each selector probes its ordered index and contributes at most limit + 1 IDs.
/// The union's first limit + 1 unique IDs must occur in these bounded prefixes,
/// even when several puzzle hashes and hints refer to the same coins.
pub(crate) fn query<'a, DB>(
    request: &WalletCoinRecordsRequest,
    height: u32,
) -> Result<QueryBuilder<DB>, StoreError>
where
    DB: Database,
    DB::Arguments: Default,
    Bytes32: Encode<'a, DB> + Type<DB>,
    i64: Encode<'a, DB> + Type<DB>,
{
    request.validate().map_err(StoreError::Corrupt)?;
    let take = i64::from(request.limit) + 1;
    let mut query = QueryBuilder::new(
        "SELECT c.confirmed_index, c.spent_index, c.coinbase, c.puzzle_hash, \
         c.coin_parent, c.amount, c.timestamp FROM coin_record AS c JOIN (\
         SELECT coin_name FROM (",
    );
    let mut seen = HashSet::new();
    for (hint, selector) in request
        .puzzle_hashes
        .iter()
        .map(|value| (false, *value))
        .chain(request.hints.iter().map(|value| (true, *value)))
    {
        if !seen.insert((hint, selector)) {
            continue;
        }
        if seen.len() > 1 {
            query.push(" UNION ALL ");
        }
        query.push("SELECT coin_name FROM (SELECT ");
        let column = if hint { "h.coin_name" } else { "r.coin_name" };
        query.push(column);
        if hint {
            query.push(
                " FROM coin_hint AS h JOIN coin_record AS r ON r.coin_name = h.coin_name \
                 WHERE h.hint = ",
            );
        } else {
            query.push(" FROM coin_record AS r WHERE r.puzzle_hash = ");
        }
        query.push_bind(selector);
        if let Some(cursor) = request.cursor {
            query
                .push(" AND ")
                .push(column)
                .push(" > ")
                .push_bind(cursor);
        }
        query
            .push(" AND r.confirmed_index <= ")
            .push_bind(i64::from(height))
            .push(" ORDER BY ")
            .push(column)
            .push(" LIMIT ")
            .push_bind(take)
            .push(") AS selector_page");
    }
    query
        .push(") AS candidates GROUP BY coin_name ORDER BY coin_name LIMIT ")
        .push_bind(take)
        .push(") AS page ON c.coin_name = page.coin_name ORDER BY c.coin_name");
    Ok(query)
}

pub(crate) fn response(
    request: &WalletCoinRecordsRequest,
    height: u32,
    mut records: Vec<CoinRecord>,
) -> WalletCoinRecordsResponse {
    let has_more = records.len() > request.limit as usize;
    records.truncate(request.limit as usize);
    for record in &mut records {
        if record.spent_block_index > height {
            record.spent_block_index = 0;
            record.spent = false;
        }
    }
    let next_cursor = if has_more {
        records.last().map(|record| record.coin.name())
    } else {
        None
    };
    WalletCoinRecordsResponse {
        success: true,
        peak_hash: request.peak_hash,
        height,
        coin_records: records,
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Execute, Row};

    #[tokio::test]
    async fn wallet_cursor_queries_use_both_selector_indexes() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for schema in [
            include_str!("../migrations/sqlite/0001_coin_record.sql"),
            include_str!("../migrations/sqlite/0003_service_indexes.sql"),
            include_str!("../migrations/sqlite/0004_hint.sql"),
        ] {
            sqlx::raw_sql(schema).execute(&pool).await.unwrap();
        }
        let request = WalletCoinRecordsRequest {
            puzzle_hashes: vec![Bytes32::from([1; 32])],
            hints: vec![Bytes32::from([2; 32])],
            peak_hash: Bytes32::default(),
            cursor: Some(Bytes32::from([3; 32])),
            limit: 500,
        };
        let mut builder = query::<sqlx::Sqlite>(&request, 5_000_000).unwrap();
        let explain = format!("EXPLAIN QUERY PLAN {}", builder.sql().as_str());
        let mut built = builder.build();
        let arguments = built.take_arguments().unwrap().unwrap();
        // The SQL comes from our parameterized builder; EXPLAIN adds only a literal prefix.
        let rows = sqlx::query_with::<sqlx::Sqlite, _>(sqlx::AssertSqlSafe(explain), arguments)
            .fetch_all(&pool)
            .await
            .unwrap();
        let plan: Vec<String> = rows.iter().map(|row| row.get("detail")).collect();
        assert!(
            plan.iter()
                .any(|step| step.contains("coin_record_summary_cursor")
                    && step.contains("puzzle_hash=?")
                    && step.contains("coin_name>?")),
            "{plan:#?}"
        );
        assert!(
            plan.iter()
                .any(|step| step.contains("SEARCH h USING PRIMARY KEY")
                    && step.contains("hint=?")
                    && step.contains("coin_name>?")),
            "{plan:#?}"
        );
        assert!(
            !plan
                .iter()
                .any(|step| step.starts_with("SCAN r") || step.starts_with("SCAN h")),
            "{plan:#?}"
        );
    }
}
