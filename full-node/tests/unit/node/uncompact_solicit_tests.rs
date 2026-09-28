use super::super::*;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use std::io::Cursor;
use tokio::sync::Mutex as TokioMutex;

// A SolicitTarget that records every delivered message and reports a fixed node type — the
// offline stand-in for a live bluebox link.
struct RecordingTarget {
    node_type: NodeType,
    sent: Arc<TokioMutex<Vec<dg_xch_core::protocols::ChiaMessage>>>,
}

#[async_trait]
impl SolicitTarget for RecordingTarget {
    async fn is_timelord(&self) -> bool {
        self.node_type == NodeType::Timelord
    }
    async fn negotiated_version(&self) -> ChiaProtocolVersion {
        ChiaProtocolVersion::default()
    }
    async fn deliver(&self, msg: dg_xch_core::protocols::ChiaMessage) -> Result<(), Error> {
        self.sent.lock().await.push(msg);
        Ok(())
    }
}

fn a_request(field_vdf: u8, tag: u8) -> RequestCompactProofOfTime {
    RequestCompactProofOfTime {
        new_proof_of_time: VdfInfo {
            challenge: Bytes32::from([tag; 32]),
            number_of_iterations: u64::from(tag) * 1000,
            output: ClassgroupElement::get_default_element(),
        },
        header_hash: Bytes32::from([tag ^ 0xFF; 32]),
        height: 5_000_000 + u32::from(tag),
        field_vdf,
    }
}

fn target(node_type: NodeType) -> RecordingTarget {
    RecordingTarget {
        node_type,
        sent: Arc::new(TokioMutex::new(Vec::new())),
    }
}

// A bulky field plus a connected TIMELORD peer means a RequestCompactProofOfTime is sent for
// that field, and it round-trips back to the exact request that was planned. A scan that only
// counted and logged would send nothing.
#[tokio::test]
async fn a_bulky_field_is_solicited_from_a_connected_timelord() {
    let reqs = vec![a_request(3 /* CC_SP */, 7)];
    let peers = vec![target(NodeType::Timelord)];
    let net = NetCounters::default();

    let sent = solicit_uncompact_from_timelords(&reqs, &peers, &net).await;
    assert_eq!(
        sent, 1,
        "the one bulky field is solicited from the timelord"
    );

    let recorded = peers[0].sent.lock().await;
    assert_eq!(recorded.len(), 1, "exactly one request message delivered");
    let msg = &recorded[0];
    assert_eq!(
        msg.msg_type,
        dg_xch_core::protocols::ProtocolMessageTypes::RequestCompactProofOfTime,
    );
    let decoded = RequestCompactProofOfTime::from_bytes(
        &mut Cursor::new(msg.data.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("wire round-trips");
    assert_eq!(decoded, reqs[0], "the sent request matches the planned one");
}

// The network-infused case: NO timelord peer connected, only a full-node peer. The scan runs,
// nothing is sent, nothing panics.
#[tokio::test]
async fn no_timelord_peer_sends_nothing_and_does_not_panic() {
    let reqs = vec![a_request(4 /* CC_IP */, 9)];
    let peers = vec![target(NodeType::FullNode)];
    let net = NetCounters::default();

    let sent = solicit_uncompact_from_timelords(&reqs, &peers, &net).await;
    assert_eq!(sent, 0, "no timelord target ⇒ no solicitation sent");
    assert!(
        peers[0].sent.lock().await.is_empty(),
        "the full-node peer is never sent a bluebox request"
    );

    // And an empty peer set is equally a no-op.
    let empty: Vec<RecordingTarget> = Vec::new();
    assert_eq!(
        solicit_uncompact_from_timelords(&reqs, &empty, &net).await,
        0,
    );
}

// An empty solicitation list is a no-op regardless of peers (the scan found nothing bulky).
#[tokio::test]
async fn an_empty_solicitation_list_sends_nothing() {
    let peers = vec![target(NodeType::Timelord)];
    let net = NetCounters::default();
    assert_eq!(solicit_uncompact_from_timelords(&[], &peers, &net).await, 0,);
    assert!(peers[0].sent.lock().await.is_empty());
}
