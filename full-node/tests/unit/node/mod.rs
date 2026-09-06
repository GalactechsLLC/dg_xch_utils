#[cfg(test)]
mod tests;

// Compact-VDF SOLICITATION send path. The scan's
// per-block plan/dedup is proven purely in full-node/tests/compact_vdf.rs (against a real mainnet
// block); here we prove the peer-conditional fan-out — the half that turns a solicitation list into
// RequestCompactProofOfTime messages on connected TIMELORD links — against a recording mock, because
// a live SocketPeer wraps a websocket sink that cannot be constructed offline.
#[cfg(test)]
mod uncompact_solicit_tests;

#[cfg(test)]
mod ub_relay_gate_tests;
