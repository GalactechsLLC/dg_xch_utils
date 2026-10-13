-- Service-tier lookups (RPC by puzzle hash / parent). Built at the sync->tip transition via
-- build_indexes, one autocommit statement per index: CONCURRENTLY cannot run inside a
-- transaction and never takes a write-blocking lock, so the confirm writer is never stalled.
CREATE INDEX CONCURRENTLY IF NOT EXISTS coin_record_puzzle_hash ON coin_record (puzzle_hash);
CREATE INDEX CONCURRENTLY IF NOT EXISTS coin_record_coin_parent ON coin_record (coin_parent);
CREATE INDEX CONCURRENTLY IF NOT EXISTS coin_record_unspent_by_ph ON coin_record (puzzle_hash) WHERE spent_index = 0;

CREATE INDEX CONCURRENTLY IF NOT EXISTS coin_record_history_page ON coin_record(puzzle_hash,confirmed_index DESC,coin_name DESC);
CREATE INDEX CONCURRENTLY IF NOT EXISTS coin_record_summary_cursor ON coin_record(puzzle_hash,coin_name);
CREATE INDEX CONCURRENTLY IF NOT EXISTS coin_record_parent_page ON coin_record(coin_parent,confirmed_index DESC,coin_name DESC);
