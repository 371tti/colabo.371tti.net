#![allow(dead_code)]

#[path = "../src/api/mod.rs"]
mod api;
#[path = "../src/auth.rs"]
mod auth;
#[path = "../src/auth_handlers.rs"]
mod auth_handlers;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/middleware.rs"]
mod middleware;
#[path = "../src/state.rs"]
mod state;

use std::{net::SocketAddr, path::PathBuf, time::Duration as StdDuration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use chrono::{Duration, Utc};
use colabo_collaboration::{Server as CollaborationServer, ServerConfig};
use colabo_domain::{Membership, Role};
use colabo_storage::{SessionRecord, Store};
use collab_core::{
    ActorId, ClientMessage, ItemId, Operation, ServerMessage, TextAnchor, Transaction, TxId,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::{Message as WsMessage, client::IntoClientRequest};
use tower::ServiceExt;
use uuid::Uuid;

type TestWebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

use crate::{auth::AuthProvider, config::Settings, state::AppState};

const ORIGIN: &str = "http://colabo.test";

struct Fixture {
    _directory: TempDir,
    app: Router,
    store: Store,
    state: AppState,
    workspace_id: Uuid,
    owner_id: Uuid,
    reader_id: Uuid,
    owner_cookie: String,
    reader_cookie: String,
    owner_csrf: String,
    reader_csrf: String,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("store")).unwrap();
        let owner = store
            .get_or_create_user(
                "https://issuer.example",
                "acceptance-owner",
                "Owner",
                None,
                None,
            )
            .await
            .unwrap();
        let reader = store
            .get_or_create_user(
                "https://issuer.example",
                "acceptance-reader",
                "Reader",
                None,
                None,
            )
            .await
            .unwrap();
        let workspace = store
            .create_workspace(owner.id, "acceptance workspace".into())
            .unwrap();
        store
            .save_memberships(
                workspace.id,
                &[Membership {
                    user_id: reader.id,
                    role: Role::Reader,
                    joined_at: Utc::now(),
                }],
            )
            .unwrap();
        store
            .create_file(workspace.id, "notes.txt", b"initial")
            .unwrap();

        let owner_csrf = "owner-csrf-for-test".to_owned();
        let reader_csrf = "reader-csrf-for-test".to_owned();
        store
            .create_session(
                "owner-session-for-test",
                &SessionRecord {
                    user_id: owner.id,
                    csrf_token: owner_csrf.clone(),
                    expires_at: Utc::now() + Duration::hours(1),
                },
            )
            .unwrap();
        store
            .create_session(
                "reader-session-for-test",
                &SessionRecord {
                    user_id: reader.id,
                    csrf_token: reader_csrf.clone(),
                    expires_at: Utc::now() + Duration::hours(1),
                },
            )
            .unwrap();

        let settings = Settings {
            bind: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            data_dir: directory.path().join("store"),
            static_dir: PathBuf::from("unused-in-api-tests"),
            issuer: "https://issuer.example".into(),
            client_id: String::new(),
            client_secret: String::new(),
            redirect_uri: format!("{ORIGIN}/auth/callback"),
            public_origin: ORIGIN.into(),
            secure_cookie: false,
        };
        let auth = AuthProvider::new(&settings).await.unwrap();
        let mut collaboration_config = ServerConfig::default();
        collaboration_config.security.allowed_origins = vec![ORIGIN.into()];
        let collaboration = CollaborationServer::with_config(collaboration_config).unwrap();
        let state = AppState::new(settings, store.clone(), collaboration, auth);
        let app: Router = api::router(state.clone()).with_state(state.clone());
        Self {
            _directory: directory,
            app,
            store,
            state,
            workspace_id: workspace.id,
            owner_id: owner.id,
            reader_id: reader.id,
            owner_cookie: "colabo_session=owner-session-for-test".into(),
            reader_cookie: "colabo_session=reader-session-for-test".into(),
            owner_csrf,
            reader_csrf,
        }
    }

    async fn request(
        &self,
        method: Method,
        uri: String,
        cookie: Option<&str>,
        csrf: Option<&str>,
        content_type: Option<&str>,
        body: Vec<u8>,
    ) -> axum::response::Response {
        let mut builder = Request::builder()
            .method(method.clone())
            .uri(uri)
            .header("origin", ORIGIN);
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", cookie);
        }
        if matches!(
            method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        ) && let Some(csrf) = csrf
        {
            builder = builder.header("x-csrf-token", csrf);
        }
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        self.app
            .clone()
            .oneshot(builder.body(Body::from(body)).unwrap())
            .await
            .unwrap()
    }

    async fn json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn workspace_path(&self, suffix: &str) -> String {
        format!("/api/workspaces/{}{suffix}", self.workspace_id)
    }
}

async fn connect_test_websocket(address: SocketAddr, path: &str, cookie: &str) -> TestWebSocket {
    let websocket_url = format!("ws://{address}{path}");
    let mut request = websocket_url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(header::ORIGIN, ORIGIN.parse().unwrap());
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    tokio::time::timeout(
        StdDuration::from_secs(2),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .expect("WebSocket handshake should complete")
    .expect("authenticated WebSocket connection should succeed")
    .0
}

async fn next_websocket_text(socket: &mut TestWebSocket) -> String {
    let frame = tokio::time::timeout(StdDuration::from_secs(2), socket.next())
        .await
        .expect("WebSocket message should arrive")
        .expect("WebSocket should remain open")
        .expect("WebSocket frame should be valid");
    let WsMessage::Text(text) = frame else {
        panic!("expected a text WebSocket frame, received {frame:?}");
    };
    text.to_string()
}

#[tokio::test]
async fn reader_http_file_checkpoint_and_upload_writes_are_rejected() {
    let fixture = Fixture::new().await;
    let file_path = fixture.workspace_path("/file?path=notes.txt");

    let read = fixture
        .request(
            Method::GET,
            file_path.clone(),
            Some(&fixture.reader_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(read.status(), StatusCode::OK);
    let body = axum::body::to_bytes(read.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body.as_ref(), b"initial");

    let collaboration = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/collaboration?path=notes.txt"),
            Some(&fixture.reader_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(collaboration.status(), StatusCode::OK);
    assert_eq!(Fixture::json(collaboration).await["can_write"], false);

    let anonymous = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/file?path=notes.txt"),
            None,
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let bad_csrf = fixture
        .request(
            Method::PUT,
            fixture.workspace_path("/file?path=notes.txt"),
            Some(&fixture.owner_cookie),
            Some("incorrect-csrf"),
            Some("text/plain"),
            b"should remain blocked".to_vec(),
        )
        .await;
    assert_eq!(bad_csrf.status(), StatusCode::FORBIDDEN);

    let write = fixture
        .request(
            Method::PUT,
            file_path,
            Some(&fixture.reader_cookie),
            Some(&fixture.reader_csrf),
            Some("text/plain"),
            b"reader edit".to_vec(),
        )
        .await;
    assert_eq!(write.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        fixture
            .store
            .read_file(fixture.workspace_id, "notes.txt")
            .unwrap(),
        b"initial"
    );
    let restored_read = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/file?path=notes.txt"),
            Some(&fixture.owner_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(restored_read.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(restored_read.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        b"initial"
    );

    let checkpoint = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/checkpoints"),
            Some(&fixture.reader_cookie),
            Some(&fixture.reader_csrf),
            Some("application/json"),
            br#"{"message":"reader checkpoint"}"#.to_vec(),
        )
        .await;
    assert_eq!(checkpoint.status(), StatusCode::FORBIDDEN);
    assert!(
        fixture
            .store
            .commit_history(fixture.workspace_id, 10)
            .unwrap()
            .is_empty()
    );

    let create_invitation = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/invitations"),
            Some(&fixture.reader_cookie),
            Some(&fixture.reader_csrf),
            Some("application/json"),
            br#"{"role":"writer"}"#.to_vec(),
        )
        .await;
    assert_eq!(create_invitation.status(), StatusCode::FORBIDDEN);
    assert!(
        fixture
            .store
            .invitations(fixture.workspace_id)
            .unwrap()
            .is_empty()
    );

    let upload = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/upload"),
            Some(&fixture.reader_cookie),
            Some(&fixture.reader_csrf),
            Some("multipart/form-data; boundary=reader-boundary"),
            multipart("reader-boundary", "blocked.txt", b"blocked"),
        )
        .await;
    assert_eq!(upload.status(), StatusCode::FORBIDDEN);
    assert!(matches!(
        fixture.store.read_file(fixture.workspace_id, "blocked.txt"),
        Err(colabo_storage::StoreError::FileMissing)
    ));
}

#[tokio::test]
async fn reader_websocket_submit_is_rejected_without_changing_document_or_file() {
    let fixture = Fixture::new().await;
    let collaboration = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/collaboration?path=notes.txt"),
            Some(&fixture.reader_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(collaboration.status(), StatusCode::OK);
    let info = Fixture::json(collaboration).await;
    assert_eq!(info["can_write"], false);
    let actor = ActorId(info["actor_id"].as_u64().unwrap());
    let document_id = collab_core::DocumentId::new(info["document_id"].as_str().unwrap()).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = fixture.app.clone();
    let test_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let websocket_url = format!("ws://{address}{}", info["websocket_path"].as_str().unwrap());
    let mut request = websocket_url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(header::ORIGIN, ORIGIN.parse().unwrap());
    request
        .headers_mut()
        .insert(header::COOKIE, fixture.reader_cookie.parse().unwrap());
    let (mut socket, _) = tokio::time::timeout(
        StdDuration::from_secs(2),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .expect("Reader WebSocket handshake should complete")
    .expect("Reader may join the document in read-only mode");

    let identity = serde_json::from_value(info["identity"].clone()).unwrap();
    let hello = ClientMessage::Hello { actor, identity };
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&hello).unwrap().into(),
        ))
        .await
        .unwrap();

    let welcome = tokio::time::timeout(StdDuration::from_secs(2), socket.next())
        .await
        .expect("server should send a welcome message")
        .unwrap()
        .unwrap();
    let WsMessage::Text(welcome) = welcome else {
        panic!("expected a welcome text message, received {welcome:?}");
    };
    assert!(matches!(
        serde_json::from_str::<ServerMessage>(&welcome).unwrap(),
        ServerMessage::Welcome { .. }
    ));

    let snapshot = fixture
        .state
        .collaboration
        .snapshot(&document_id)
        .await
        .unwrap();
    let left = snapshot.document.visible_ids().last().copied();
    let transaction = Transaction::new(
        TxId::new(actor, 1),
        vec![Operation::TextInsert {
            start: ItemId::new(actor, 2),
            anchor: TextAnchor { left, right: None },
            text: " forbidden".into(),
        }],
    );
    let tx_id = transaction.id;
    let submit = ClientMessage::Submit { transaction };
    socket
        .send(WsMessage::Text(
            serde_json::to_string(&submit).unwrap().into(),
        ))
        .await
        .unwrap();
    let rejected = tokio::time::timeout(StdDuration::from_secs(2), socket.next())
        .await
        .expect("server should answer the attempted edit")
        .unwrap()
        .unwrap();
    let WsMessage::Text(rejected) = rejected else {
        panic!("expected a rejection text message, received {rejected:?}");
    };
    match serde_json::from_str::<ServerMessage>(&rejected).unwrap() {
        ServerMessage::Rejected {
            tx_id: rejected_id,
            reason,
        } => {
            assert_eq!(rejected_id, tx_id);
            assert!(reason.contains("read-only"));
        }
        other => panic!("expected read-only rejection, received {other:?}"),
    }

    assert_eq!(
        fixture
            .state
            .collaboration
            .snapshot(&document_id)
            .await
            .unwrap()
            .document
            .text(),
        "initial"
    );
    assert_eq!(
        fixture
            .store
            .read_file(fixture.workspace_id, "notes.txt")
            .unwrap(),
        b"initial"
    );
    let _ = socket.close(None).await;
    test_server.abort();
}

#[tokio::test]
async fn owner_and_writer_websockets_exchange_presence_and_persist_each_others_edits() {
    let fixture = Fixture::new().await;
    let writer = fixture
        .store
        .get_or_create_user(
            "https://issuer.example",
            "acceptance-writer",
            "Writer",
            None,
            None,
        )
        .await
        .unwrap();
    let mut memberships = fixture.store.memberships(fixture.workspace_id).unwrap();
    memberships.push(Membership {
        user_id: writer.id,
        role: Role::Writer,
        joined_at: Utc::now(),
    });
    fixture
        .store
        .save_memberships(fixture.workspace_id, &memberships)
        .unwrap();
    fixture
        .store
        .create_session(
            "writer-session-for-test",
            &SessionRecord {
                user_id: writer.id,
                csrf_token: "writer-csrf-for-test".into(),
                expires_at: Utc::now() + Duration::hours(1),
            },
        )
        .unwrap();
    let writer_cookie = "colabo_session=writer-session-for-test";

    let owner_info_response = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/collaboration?path=notes.txt"),
            Some(&fixture.owner_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(owner_info_response.status(), StatusCode::OK);
    let owner_info = Fixture::json(owner_info_response).await;
    assert_eq!(owner_info["can_write"], true);
    let writer_info_response = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/collaboration?path=notes.txt"),
            Some(writer_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(writer_info_response.status(), StatusCode::OK);
    let writer_info = Fixture::json(writer_info_response).await;
    assert_eq!(writer_info["can_write"], true);
    assert_eq!(writer_info["document_id"], owner_info["document_id"]);

    let document_id =
        collab_core::DocumentId::new(owner_info["document_id"].as_str().unwrap()).unwrap();
    let owner_actor = ActorId(owner_info["actor_id"].as_u64().unwrap());
    let writer_actor = ActorId(writer_info["actor_id"].as_u64().unwrap());
    assert_ne!(owner_actor, writer_actor);
    let owner_identity = serde_json::from_value(owner_info["identity"].clone()).unwrap();
    let writer_identity = serde_json::from_value(writer_info["identity"].clone()).unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = fixture.app.clone();
    let test_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut owner_socket = connect_test_websocket(
        address,
        owner_info["websocket_path"].as_str().unwrap(),
        &fixture.owner_cookie,
    )
    .await;
    let mut writer_socket = connect_test_websocket(
        address,
        writer_info["websocket_path"].as_str().unwrap(),
        writer_cookie,
    )
    .await;

    for (socket, actor, identity) in [
        (&mut owner_socket, owner_actor, owner_identity),
        (&mut writer_socket, writer_actor, writer_identity),
    ] {
        let hello = ClientMessage::Hello { actor, identity };
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&hello).unwrap().into(),
            ))
            .await
            .unwrap();
        let welcome: ServerMessage =
            serde_json::from_str(&next_websocket_text(socket).await).unwrap();
        assert!(matches!(welcome, ServerMessage::Welcome { .. }));
    }

    let initial = fixture
        .state
        .collaboration
        .snapshot(&document_id)
        .await
        .unwrap();
    let cursor = TextAnchor {
        left: initial.document.visible_ids().last().copied(),
        right: None,
    };
    let presence = ClientMessage::SessionData {
        data: [
            ("cursor".to_owned(), serde_json::to_value(cursor).unwrap()),
            ("selection".to_owned(), Value::Null),
        ]
        .into(),
    };
    writer_socket
        .send(WsMessage::Text(
            serde_json::to_string(&presence).unwrap().into(),
        ))
        .await
        .unwrap();
    match serde_json::from_str::<ServerMessage>(&next_websocket_text(&mut owner_socket).await)
        .unwrap()
    {
        ServerMessage::SessionDataUpdate { actor, data, .. } => {
            assert_eq!(actor, writer_actor);
            assert_eq!(data["cursor"], serde_json::to_value(cursor).unwrap());
            assert_eq!(data["selection"], Value::Null);
        }
        other => panic!("expected Writer presence update, received {other:?}"),
    }

    let writer_transaction = Transaction::new(
        TxId::new(writer_actor, 1),
        vec![Operation::TextInsert {
            start: ItemId::new(writer_actor, 2),
            anchor: TextAnchor {
                left: initial.document.visible_ids().last().copied(),
                right: None,
            },
            text: "-writer".into(),
        }],
    );
    let writer_tx_id = writer_transaction.id;
    writer_socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientMessage::Submit {
                transaction: writer_transaction,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    match serde_json::from_str::<ServerMessage>(&next_websocket_text(&mut writer_socket).await)
        .unwrap()
    {
        ServerMessage::Committed { committed } => {
            assert_eq!(committed.transaction.id, writer_tx_id);
        }
        other => panic!("Writer did not receive its own commit echo: {other:?}"),
    }
    match serde_json::from_str::<ServerMessage>(&next_websocket_text(&mut owner_socket).await)
        .unwrap()
    {
        ServerMessage::Committed { committed } => {
            assert_eq!(committed.transaction.id, writer_tx_id);
        }
        other => panic!("Owner did not receive Writer commit: {other:?}"),
    }

    let after_writer = fixture
        .state
        .collaboration
        .snapshot(&document_id)
        .await
        .unwrap();
    let owner_transaction = Transaction::new(
        TxId::new(owner_actor, 1),
        vec![Operation::TextInsert {
            start: ItemId::new(owner_actor, 2),
            anchor: TextAnchor {
                left: after_writer.document.visible_ids().last().copied(),
                right: None,
            },
            text: "-owner".into(),
        }],
    );
    let owner_tx_id = owner_transaction.id;
    owner_socket
        .send(WsMessage::Text(
            serde_json::to_string(&ClientMessage::Submit {
                transaction: owner_transaction,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    match serde_json::from_str::<ServerMessage>(&next_websocket_text(&mut writer_socket).await)
        .unwrap()
    {
        ServerMessage::Committed { committed } => {
            assert_eq!(committed.transaction.id, owner_tx_id);
        }
        other => panic!("Writer did not receive Owner commit: {other:?}"),
    }

    let expected = b"initial-writer-owner";
    assert_eq!(
        fixture
            .state
            .collaboration
            .snapshot(&document_id)
            .await
            .unwrap()
            .document
            .text(),
        "initial-writer-owner"
    );
    tokio::time::timeout(StdDuration::from_secs(2), async {
        loop {
            if fixture
                .store
                .read_file(fixture.workspace_id, "notes.txt")
                .is_ok_and(|contents| contents == expected)
            {
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(10)).await;
        }
    })
    .await
    .expect("both accepted edits should reach the persisted file");

    let _ = owner_socket.close(None).await;
    let _ = writer_socket.close(None).await;
    test_server.abort();
}

#[tokio::test]
async fn invitation_http_flow_is_reusable_role_scoped_and_revocable() {
    let fixture = Fixture::new().await;
    let created = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/invitations"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("application/json"),
            serde_json::to_vec(&json!({"role":"writer"})).unwrap(),
        )
        .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created = Fixture::json(created).await;
    let invitation_id = created["invitation"]["id"].as_str().unwrap();
    let path = created["path"].as_str().unwrap();
    let token = path.rsplit('/').next().unwrap();
    assert!(created["invitation"].get("token_hash").is_none());
    assert_ne!(
        fixture.store.invitations(fixture.workspace_id).unwrap()[0].token_hash,
        token
    );

    let preview = fixture
        .request(
            Method::GET,
            format!("/api/invites/{}/{}", fixture.workspace_id, token),
            None,
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(preview.status(), StatusCode::OK);
    assert_eq!(Fixture::json(preview).await["role"], "writer");

    let joiner = fixture
        .store
        .get_or_create_user(
            "https://issuer.example",
            "acceptance-joiner",
            "Joiner",
            None,
            None,
        )
        .await
        .unwrap();
    let joiner_csrf = "joiner-csrf-for-test";
    fixture
        .store
        .create_session(
            "joiner-session-for-test",
            &SessionRecord {
                user_id: joiner.id,
                csrf_token: joiner_csrf.into(),
                expires_at: Utc::now() + Duration::hours(1),
            },
        )
        .unwrap();
    let accepted = fixture
        .request(
            Method::POST,
            format!("/api/invites/{}/{token}/accept", fixture.workspace_id),
            Some("colabo_session=joiner-session-for-test"),
            Some(joiner_csrf),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(Fixture::json(accepted).await["joined"], true);
    assert_eq!(
        fixture
            .store
            .memberships(fixture.workspace_id)
            .unwrap()
            .iter()
            .find(|member| member.user_id == joiner.id)
            .unwrap()
            .role,
        Role::Writer
    );

    let second_joiner = fixture
        .store
        .get_or_create_user(
            "https://issuer.example",
            "acceptance-second-joiner",
            "Second joiner",
            None,
            None,
        )
        .await
        .unwrap();
    let second_csrf = "second-joiner-csrf-for-test";
    fixture
        .store
        .create_session(
            "second-joiner-session-for-test",
            &SessionRecord {
                user_id: second_joiner.id,
                csrf_token: second_csrf.into(),
                expires_at: Utc::now() + Duration::hours(1),
            },
        )
        .unwrap();
    let accepted_again = fixture
        .request(
            Method::POST,
            format!("/api/invites/{}/{token}/accept", fixture.workspace_id),
            Some("colabo_session=second-joiner-session-for-test"),
            Some(second_csrf),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(accepted_again.status(), StatusCode::OK);
    assert_eq!(Fixture::json(accepted_again).await["joined"], true);
    assert!(
        fixture
            .store
            .memberships(fixture.workspace_id)
            .unwrap()
            .iter()
            .any(|member| member.user_id == second_joiner.id && member.role == Role::Writer)
    );

    let revoked = fixture
        .request(
            Method::DELETE,
            fixture.workspace_path(&format!("/invitations/{invitation_id}")),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(revoked.status(), StatusCode::OK);
    let preview_after_revoke = fixture
        .request(
            Method::GET,
            format!("/api/invites/{}/{token}", fixture.workspace_id),
            None,
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(preview_after_revoke.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn owner_can_create_workspace_and_reader_can_send_persistent_chat() {
    let fixture = Fixture::new().await;
    let created = fixture
        .request(
            Method::POST,
            "/api/workspaces".into(),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("application/json"),
            br#"{"name":"new workspace"}"#.to_vec(),
        )
        .await;
    assert_eq!(created.status(), StatusCode::OK);
    let created = Fixture::json(created).await;
    let new_workspace_id = created["id"].as_str().unwrap();
    let listed = fixture
        .request(
            Method::GET,
            "/api/workspaces".into(),
            Some(&fixture.owner_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(listed.status(), StatusCode::OK);
    assert!(
        Fixture::json(listed)
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|workspace| workspace["id"] == new_workspace_id)
    );

    let sent = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/chat"),
            Some(&fixture.reader_cookie),
            Some(&fixture.reader_csrf),
            Some("application/json"),
            br#"{"body":"  reader can chat  "}"#.to_vec(),
        )
        .await;
    assert_eq!(sent.status(), StatusCode::OK);
    let sent = Fixture::json(sent).await;
    assert_eq!(sent["body"], "reader can chat");
    assert_eq!(sent["user_id"], fixture.reader_id.to_string());

    let history = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/chat"),
            Some(&fixture.reader_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(history.status(), StatusCode::OK);
    let history = Fixture::json(history).await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["body"], "reader can chat");
}

#[tokio::test]
async fn chat_http_post_is_delivered_to_owner_and_reader_websockets_and_persisted() {
    let fixture = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = fixture.app.clone();
    let test_server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let chat_path = fixture.workspace_path("/chat/ws");
    let mut owner_socket = connect_test_websocket(address, &chat_path, &fixture.owner_cookie).await;
    let mut reader_socket =
        connect_test_websocket(address, &chat_path, &fixture.reader_cookie).await;

    let sent = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/chat"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("application/json"),
            br#"{"body":"message delivered to both sockets"}"#.to_vec(),
        )
        .await;
    assert_eq!(sent.status(), StatusCode::OK);
    let sent = Fixture::json(sent).await;

    for (name, socket) in [
        (&"Owner", &mut owner_socket),
        (&"Reader", &mut reader_socket),
    ] {
        let event: Value = serde_json::from_str(&next_websocket_text(socket).await).unwrap();
        assert_eq!(event["type"], "message", "{name} socket event type");
        assert_eq!(
            event["message"]["id"], sent["id"],
            "{name} receives posted message"
        );
        assert_eq!(
            event["message"]["body"], "message delivered to both sockets",
            "{name} receives message body"
        );
        assert_eq!(event["message"]["user_id"], fixture.owner_id.to_string());
    }

    let history = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/chat"),
            Some(&fixture.reader_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(history.status(), StatusCode::OK);
    let history = Fixture::json(history).await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["id"], sent["id"]);
    assert_eq!(history[0]["body"], "message delivered to both sockets");

    let _ = owner_socket.close(None).await;
    let _ = reader_socket.close(None).await;
    test_server.abort();
}

#[tokio::test]
async fn owner_restore_route_keeps_history_and_creates_restore_checkpoint() {
    let fixture = Fixture::new().await;
    let first = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/checkpoints"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("application/json"),
            br#"{"message":"first"}"#.to_vec(),
        )
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first = Fixture::json(first).await;
    let first_id = first["id"].as_str().unwrap().to_owned();

    let historical_file = fixture
        .request(
            Method::GET,
            fixture.workspace_path(&format!("/checkpoints/{first_id}/file?path=notes.txt")),
            Some(&fixture.reader_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(historical_file.status(), StatusCode::OK);
    assert_eq!(
        axum::body::to_bytes(historical_file.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        b"initial"
    );

    let edit = fixture
        .request(
            Method::PUT,
            fixture.workspace_path("/file?path=notes.txt"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("text/plain"),
            b"changed after checkpoint".to_vec(),
        )
        .await;
    assert_eq!(edit.status(), StatusCode::OK);

    let reader_restore = fixture
        .request(
            Method::POST,
            fixture.workspace_path(&format!("/checkpoints/{first_id}/restore")),
            Some(&fixture.reader_cookie),
            Some(&fixture.reader_csrf),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(reader_restore.status(), StatusCode::FORBIDDEN);

    let restore = fixture
        .request(
            Method::POST,
            fixture.workspace_path(&format!("/checkpoints/{first_id}/restore")),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(restore.status(), StatusCode::OK);
    let restore_result = Fixture::json(restore).await;
    assert_eq!(restore_result["files_restored"], 1);
    assert!(restore_result["checkpoint"].is_object());
    assert_eq!(
        fixture
            .store
            .read_file(fixture.workspace_id, "notes.txt")
            .unwrap(),
        b"initial"
    );

    let history = fixture
        .store
        .commit_history(fixture.workspace_id, 10)
        .unwrap();
    assert_eq!(
        history.len(),
        3,
        "restore must append rather than reset history"
    );
    assert_ne!(history[0].id, first_id);
    assert_eq!(
        fixture
            .store
            .read_checkpoint_file(fixture.workspace_id, &history[1].id, "notes.txt")
            .unwrap(),
        b"changed after checkpoint"
    );
    assert_eq!(
        fixture
            .store
            .read_checkpoint_file(fixture.workspace_id, &first_id, "notes.txt")
            .unwrap(),
        b"initial"
    );

    let traversal = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/upload"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("multipart/form-data; boundary=traversal-boundary"),
            multipart("traversal-boundary", "../../outside.bin", b"unsafe"),
        )
        .await;
    assert_eq!(traversal.status(), StatusCode::BAD_REQUEST);
    assert!(matches!(
        fixture.store.read_file(fixture.workspace_id, "outside.bin"),
        Err(colabo_storage::StoreError::FileMissing)
    ));
}

#[tokio::test]
async fn owner_http_upload_streams_binary_payload_into_nested_workspace_path() {
    let fixture = Fixture::new().await;
    let payload: Vec<u8> = (0usize..128 * 1024)
        .map(|index| (index.wrapping_mul(37) ^ (index >> 5)) as u8)
        .collect();
    let path = "deeply/nested/uploads/bytes.bin";
    let response = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/upload"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("multipart/form-data; boundary=upload-boundary"),
            multipart("upload-boundary", path, &payload),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        fixture.store.read_file(fixture.workspace_id, path).unwrap(),
        payload
    );
}

#[tokio::test]
async fn reader_downloads_uploaded_nested_binary_file_with_exact_bytes() {
    let fixture = Fixture::new().await;
    let payload: Vec<u8> = (0usize..16 * 1024)
        .map(|index| (index.wrapping_mul(53) ^ (index >> 3)) as u8)
        .collect();
    let path = "nested/downloads/document.bin";
    let uploaded = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/upload"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("multipart/form-data; boundary=download-boundary"),
            multipart("download-boundary", path, &payload),
        )
        .await;
    assert_eq!(uploaded.status(), StatusCode::OK);

    let downloaded = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/download?path=nested/downloads/document.bin"),
            Some(&fixture.reader_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(
        downloaded
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/octet-stream")
    );
    assert_eq!(
        axum::body::to_bytes(downloaded.into_body(), usize::MAX)
            .await
            .unwrap()
            .as_ref(),
        payload
    );
}

#[tokio::test]
async fn large_upload_replaces_an_active_collaborative_file_and_persists_bytes() {
    let fixture = Fixture::new().await;
    let path = "notes.txt";
    let collaboration_info = fixture
        .request(
            Method::GET,
            fixture.workspace_path("/collaboration?path=notes.txt"),
            Some(&fixture.owner_cookie),
            None,
            None,
            Vec::new(),
        )
        .await;
    assert_eq!(collaboration_info.status(), StatusCode::OK);
    assert!(
        fixture
            .state
            .is_document_active(
                fixture
                    .store
                    .collaboration_file(fixture.workspace_id, path)
                    .unwrap()
                    .id
            )
            .await
    );
    let mut events = fixture
        .state
        .workspace_event_sender(fixture.workspace_id)
        .await
        .subscribe();

    let payload = vec![b'x'; 24 * 1024 * 1024 + 1];
    let uploaded = fixture
        .request(
            Method::POST,
            fixture.workspace_path("/upload"),
            Some(&fixture.owner_cookie),
            Some(&fixture.owner_csrf),
            Some("multipart/form-data; boundary=large-replacement-boundary"),
            multipart("large-replacement-boundary", path, &payload),
        )
        .await;
    assert_eq!(uploaded.status(), StatusCode::OK);

    let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("upload should publish a workspace event")
        .unwrap();
    assert_eq!(event.kind, "file_replaced");
    assert_eq!(event.path.as_deref(), Some(path));
    assert_eq!(
        fixture.store.read_file(fixture.workspace_id, path).unwrap(),
        payload
    );
}

fn multipart(boundary: &str, path: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file_path\"\r\n\r\n{path}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"upload.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(payload);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}
