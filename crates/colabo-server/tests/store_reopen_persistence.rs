use chrono::{Duration, Utc};
use colabo_domain::{Invitation, Membership, Role};
use colabo_storage::Store;
use uuid::Uuid;

#[tokio::test]
async fn store_reopen_preserves_workspace_content_and_history() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let store = Store::open(root).unwrap();

    let owner = store
        .get_or_create_user(
            "https://issuer.example",
            "owner-subject",
            "Workspace owner",
            Some("owner"),
            None,
        )
        .await
        .unwrap();
    let workspace = store
        .create_workspace(owner.id, "persistent workspace".into())
        .unwrap();
    let member_id = Uuid::now_v7();
    store
        .save_memberships(
            workspace.id,
            &[Membership {
                user_id: member_id,
                role: Role::Writer,
                joined_at: Utc::now(),
            }],
        )
        .unwrap();
    let invitation_id = Uuid::now_v7();
    store
        .save_invitations(
            workspace.id,
            &[Invitation {
                id: invitation_id,
                token_hash: "test-only-hash".into(),
                role: Role::Reader,
                issued_by: owner.id,
                created_at: Utc::now(),
                expires_at: Utc::now() + Duration::days(7),
                revoked_at: None,
            }],
        )
        .unwrap();

    store.create_directory(workspace.id, "docs").unwrap();
    store
        .create_file(workspace.id, "docs/notes.txt", b"checkpoint one")
        .unwrap();
    store
        .append_chat_message(workspace.id, owner.id, "persisted chat".into())
        .unwrap();
    let first_checkpoint = store
        .checkpoint(workspace.id, "first", owner.id, &owner.display_name)
        .unwrap()
        .expect("first checkpoint should be created");

    store
        .replace_file(workspace.id, "docs/notes.txt", b"checkpoint two")
        .unwrap();
    let second_checkpoint = store
        .checkpoint(workspace.id, "second", owner.id, &owner.display_name)
        .unwrap()
        .expect("second checkpoint should be created");

    drop(store);

    let reopened = Store::open(root).unwrap();
    let reopened_owner = reopened
        .get_or_create_user(
            "https://issuer.example",
            "owner-subject",
            "Workspace owner",
            Some("owner"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(reopened_owner.id, owner.id);
    assert_eq!(
        reopened.workspace(workspace.id).unwrap().name,
        workspace.name
    );
    assert_eq!(
        reopened.read_file(workspace.id, "docs/notes.txt").unwrap(),
        b"checkpoint two"
    );
    assert_eq!(
        reopened
            .memberships(workspace.id)
            .unwrap()
            .iter()
            .find(|membership| membership.user_id == member_id)
            .map(|membership| membership.role),
        Some(Role::Writer)
    );
    let invitations = reopened.invitations(workspace.id).unwrap();
    assert_eq!(invitations.len(), 1);
    assert_eq!(invitations[0].id, invitation_id);
    assert_eq!(invitations[0].role, Role::Reader);
    assert_eq!(
        reopened.chat_messages(workspace.id, 0, 20).unwrap()[0].body,
        "persisted chat"
    );

    let history = reopened.commit_history(workspace.id, 20).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].id, second_checkpoint.id);
    assert_eq!(history[1].id, first_checkpoint.id);
    assert_eq!(
        reopened
            .read_checkpoint_file(workspace.id, &first_checkpoint.id, "docs/notes.txt")
            .unwrap(),
        b"checkpoint one"
    );
    assert_eq!(
        reopened
            .read_checkpoint_file(workspace.id, &second_checkpoint.id, "docs/notes.txt")
            .unwrap(),
        b"checkpoint two"
    );
}
