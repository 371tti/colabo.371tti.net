use std::collections::HashSet;

use colabo_collaboration::Server;
use collab_core::{ActorId, ItemId, Operation, TextAnchor, Transaction, TxId};

#[tokio::test]
async fn concurrent_client_edits_are_broadcast_and_merged_into_one_document() {
    let server = Server::new();
    let document_id = server.create_session("base").await.unwrap();
    let mut committed_events = server.subscribe(&document_id).await.unwrap();
    let snapshot = server.snapshot(&document_id).await.unwrap();
    let left = snapshot.document.visible_ids().last().copied();

    let client_a = ActorId(21);
    let client_b = ActorId(22);
    let transaction_a = Transaction::new(
        TxId::new(client_a, 1),
        vec![Operation::TextInsert {
            start: ItemId::new(client_a, 2),
            anchor: TextAnchor { left, right: None },
            text: " from A".into(),
        }],
    );
    let transaction_b = Transaction::new(
        TxId::new(client_b, 1),
        vec![Operation::TextInsert {
            start: ItemId::new(client_b, 2),
            anchor: TextAnchor { left, right: None },
            text: " from B".into(),
        }],
    );

    let (result_a, result_b) = tokio::join!(
        server.submit_to(&document_id, client_a, transaction_a),
        server.submit_to(&document_id, client_b, transaction_b),
    );
    let mut sequences = [result_a.unwrap().seq, result_b.unwrap().seq];
    sequences.sort_unstable();
    assert_eq!(sequences, [2, 3]);

    let mut observed_actors = HashSet::new();
    for expected_seq in [2, 3] {
        let committed =
            tokio::time::timeout(std::time::Duration::from_secs(1), committed_events.recv())
                .await
                .expect("each accepted client edit should be broadcast")
                .unwrap();
        assert_eq!(committed.seq, expected_seq);
        observed_actors.insert(committed.transaction.id.actor);
    }
    assert_eq!(observed_actors, HashSet::from([client_a, client_b]));

    let merged = server.snapshot(&document_id).await.unwrap();
    let text = merged.document.text();
    assert_eq!(merged.seq, 3);
    assert!(text.starts_with("base"));
    assert!(text.contains(" from A"));
    assert!(text.contains(" from B"));
}
