CREATE INDEX IF NOT EXISTS coin_record_puzzle_hash ON coin_record (puzzle_hash);
CREATE INDEX IF NOT EXISTS coin_record_coin_parent ON coin_record (coin_parent);
CREATE INDEX IF NOT EXISTS coin_record_unspent_by_ph ON coin_record (puzzle_hash) WHERE spent_index = 0;

CREATE INDEX IF NOT EXISTS coin_record_history_page ON coin_record(puzzle_hash,confirmed_index DESC,coin_name DESC);
CREATE INDEX IF NOT EXISTS coin_record_summary_cursor ON coin_record(puzzle_hash,coin_name);
CREATE INDEX IF NOT EXISTS coin_record_parent_page ON coin_record(coin_parent,confirmed_index DESC,coin_name DESC);
