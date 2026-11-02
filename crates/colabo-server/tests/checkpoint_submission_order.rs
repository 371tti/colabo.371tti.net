use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use colabo_collaboration::Server as CollaborationServer;
use colabo_storage::Store;
use collab_core::{ActorId, ItemId, Operation, TextAnchor, Transaction, TxId};
use uuid::Uuid;

async fn flush_document(
    collaboration: &CollaborationServer,
    store: &Store,
    workspace_id: Uuid,
    file_id: Uuid,
    document_id: &collab_core::DocumentId,
) {
    let snapshot = collaboration.snapshot(document_id).await.unwrap();
    let text = snapshot.document.text();
    store
        .save_collaboration_state(workspace_id, file_id, &snapshot, &text)
        .unwrap();
    store.sync_collaboration_files(workspace_id).unwrap();
}

#[tokio::test]
async fn checkpoint_waits_for_collaborative_submit_and_persists_it() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let owner_id = Uuid::now_v7();
    let workspace = store
        .create_workspace(owner_id, "checkpoint ordering".into())
        .unwrap();
    store
        .create_file(workspace.id, "notes.txt", b"base")
        .unwrap();
    let file = store.collaboration_file(workspace.id, "notes.txt").unwrap();
    let document_id = collab_core::DocumentId::new(file.id.to_string()).unwrap();
    let collaboration = CollaborationServer::new();
    collaboration
        .create_session_with_id(document_id.clone(), "base")
        .await
        .unwrap();

    let snapshot = collaboration.snapshot(&document_id).await.unwrap();
    store
        .save_collaboration_state(workspace.id, file.id, &snapshot, "base")
        .unwrap();

    let persister_store = store.clone();
    collaboration
        .set_snapshot_persister(&document_id, move |snapshot| {
            let store = persister_store.clone();
            async move {
                store
                    .save_collaboration_snapshot(workspace.id, file.id, &snapshot)
                    .map_err(|error| error.to_string())
            }
        })
        .await
        .unwrap();

    let guard_requested = Arc::new(AtomicBool::new(false));
    let guard_store = store.clone();
    let guard_requested_from_factory = guard_requested.clone();
    collaboration
        .set_submission_guard(&document_id, move || {
            let store = guard_store.clone();
            let guard_requested = guard_requested_from_factory.clone();
            async move {
                guard_requested.store(true, Ordering::Release);
                store.lock_workspace(workspace.id).await
            }
        })
        .await
        .unwrap();

    let workspace_guard = store.lock_workspace(workspace.id).await;
    let actor = ActorId(7);
    let before = collaboration.snapshot(&document_id).await.unwrap();
    let left = before.document.visible_ids().last().copied();
    let transaction = Transaction::new(
        TxId::new(actor, 1),
        vec![Operation::TextInsert {
            start: ItemId::new(actor, 2),
            anchor: TextAnchor { left, right: None },
            text: "!".into(),
        }],
    );
    let submit_server = collaboration.clone();
    let submit_document_id = document_id.clone();
    let mut submit = tokio::spawn(async move {
        submit_server
            .submit_to(&submit_document_id, actor, transaction)
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        while !guard_requested.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("submission should attempt to acquire the workspace guard");
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut submit)
            .await
            .is_err()
    );

    flush_document(&collaboration, &store, workspace.id, file.id, &document_id).await;
    let checkpoint_a = store
        .checkpoint(workspace.id, "before submit", owner_id, "Owner")
        .unwrap()
        .expect("initial checkpoint should be created");
    assert_eq!(
        store
            .read_checkpoint_file(workspace.id, &checkpoint_a.id, "notes.txt")
            .unwrap(),
        b"base"
    );

    drop(workspace_guard);
    submit.await.unwrap().unwrap();

    let workspace_guard = store.lock_workspace(workspace.id).await;
    flush_document(&collaboration, &store, workspace.id, file.id, &document_id).await;
    let checkpoint_b = store
        .checkpoint(workspace.id, "after submit", owner_id, "Owner")
        .unwrap()
        .expect("checkpoint after the committed edit should be created");
    assert_eq!(
        store
            .read_checkpoint_file(workspace.id, &checkpoint_b.id, "notes.txt")
            .unwrap(),
        b"base!"
    );
    drop(workspace_guard);

    let upload_guard = store.lock_workspace(workspace.id).await;
    collaboration
        .replace_text_with_guard_held(&document_id, "uploaded")
        .await
        .unwrap();
    flush_document(&collaboration, &store, workspace.id, file.id, &document_id).await;
    assert_eq!(
        store.read_file(workspace.id, "notes.txt").unwrap(),
        b"uploaded"
    );
    drop(upload_guard);
}
