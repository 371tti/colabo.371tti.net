use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::extract::ws::{Message, WebSocket};
use collab_core::{ClientMessage, DocumentId, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use tokio::{sync::broadcast, time::timeout};

use super::{
    ConnectionSlot, DocumentRoom, Server, ServerError, SessionDataEvent, session_data_snapshot,
    submit_to_room, update_session_data, validate_client_actor, welcome_from_room,
    without_live_position,
};

static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct AuthorizedSocket {
    pub(super) server: Server,
    pub(super) document_id: DocumentId,
    pub(super) document_room: Arc<DocumentRoom>,
    pub(super) connection_slot: ConnectionSlot,
    pub(super) actor: collab_core::ActorId,
    pub(super) identity: collab_core::EditorIdentity,
    pub(super) writable: bool,
}

pub(super) async fn handle_authorized_socket(socket: WebSocket, authorized: AuthorizedSocket) {
    let AuthorizedSocket {
        server,
        document_id,
        document_room,
        connection_slot: _connection_slot,
        actor: authorized_actor,
        identity: authenticated_identity,
        writable,
    } = authorized;
    let (mut sender, mut receiver) = socket.split();
    // Subscribe before taking the initial snapshot so no committed event can fall in between.
    let mut committed_rx = document_room.committed_tx.subscribe();
    let mut closed_rx = document_room.closed_tx.subscribe();
    let mut session_data_rx = document_room.session_data_tx.subscribe();
    let requested_identity = match timeout(Duration::from_secs(10), receiver.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => {
            match serde_json::from_str::<ClientMessage>(text.as_str()) {
                Ok(ClientMessage::Hello { actor, identity }) => (actor, identity),
                _ => {
                    let _ = send_server(
                        &mut sender,
                        &ServerMessage::ProtocolError {
                            message: "the first message must be hello".into(),
                        },
                    )
                    .await;
                    return;
                }
            }
        }
        _ => return,
    };
    if requested_identity.0 != authorized_actor {
        let _ = send_server(
            &mut sender,
            &ServerMessage::ProtocolError {
                message: "the connected actor does not match the authenticated user".into(),
            },
        )
        .await;
        return;
    }
    let actor = authorized_actor;
    let identity = authenticated_identity;
    if let Err(error) = validate_client_actor(actor) {
        let _ = send_server(
            &mut sender,
            &ServerMessage::ProtocolError {
                message: error.to_string(),
            },
        )
        .await;
        return;
    }
    if let Err(error) = server.join_session(&document_id, &document_room).await {
        let message = match &error {
            ServerError::SessionExpired => ServerMessage::SessionExpired {
                message: "this editing session has expired".into(),
            },
            ServerError::ConnectionLimit => ServerMessage::ProtocolError {
                message: error.to_string(),
            },
            _ => ServerMessage::ProtocolError {
                message: "could not join the editing session".into(),
            },
        };
        let _ = send_server(&mut sender, &message).await;
        return;
    }

    let connection_id = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed);
    let (snapshot, undo_stack, redo_stack) = welcome_from_room(&document_room, actor).await;
    let session_data = session_data_snapshot(&document_room, actor, &identity.id).await;
    let mut last_seq = snapshot.seq;
    if send_server(
        &mut sender,
        &ServerMessage::Welcome {
            snapshot,
            max_undo_transactions_per_actor: server
                .inner
                .config
                .limits
                .max_undo_transactions_per_actor,
            undo_stack,
            redo_stack,
            session_data,
        },
    )
    .await
    .is_err()
    {
        server.leave_session(&document_id, &document_room).await;
        return;
    }

    loop {
        tokio::select! {
            inbound = receiver.next() => {
                let Some(Ok(message)) = inbound else { break; };
                match message {
                    Message::Text(text) => {
                        let message = match serde_json::from_str::<ClientMessage>(text.as_str()) {
                            Ok(message) => message,
                            Err(error) => {
                                let response = ServerMessage::ProtocolError {
                                    message: format!("invalid message: {error}"),
                                };
                                if send_server(&mut sender, &response).await.is_err() { break; }
                                continue;
                            }
                        };
                        match message {
                            ClientMessage::Submit { transaction } => {
                                let tx_id = transaction.id;
                                if !writable {
                                    if send_server(
                                        &mut sender,
                                        &ServerMessage::Rejected {
                                            tx_id,
                                            reason: "this workspace role is read-only".into(),
                                        },
                                    ).await.is_err() {
                                        break;
                                    }
                                    continue;
                                }
                                if let Err(error) = submit_to_room(&document_room, actor, transaction).await
                                    && send_server(
                                        &mut sender,
                                        &ServerMessage::Rejected {
                                            tx_id,
                                            reason: error.to_string(),
                                        },
                                    )
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            ClientMessage::SessionData { data } => {
                                match update_session_data(
                                    &document_room,
                                    connection_id,
                                    actor,
                                    identity.clone(),
                                    data.clone(),
                                )
                                .await
                                {
                                    Ok(()) => {
                                        let _ = document_room.session_data_tx.send(SessionDataEvent {
                                            connection_id,
                                            actor,
                                            identity: identity.clone(),
                                            data: Some(data),
                                        });
                                    }
                                    Err(error) => {
                                        if send_server(
                                            &mut sender,
                                            &ServerMessage::ProtocolError {
                                                message: error.to_string(),
                                            },
                                        )
                                        .await
                                        .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                            ClientMessage::Hello { .. } => {
                                let response = ServerMessage::ProtocolError {
                                    message: "hello may only be sent once".into(),
                                };
                                if send_server(&mut sender, &response).await.is_err() { break; }
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
                }
            }
            event = committed_rx.recv() => {
                match event {
                    Ok(committed) if committed.seq > last_seq => {
                        last_seq = committed.seq;
                        if send_server(&mut sender, &ServerMessage::Committed { committed }).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let (snapshot, undo_stack, redo_stack) = welcome_from_room(&document_room, actor).await;
                        let session_data = session_data_snapshot(&document_room, actor, &identity.id).await;
                        last_seq = snapshot.seq;
                        if send_server(&mut sender, &ServerMessage::Welcome {
                            snapshot,
                            max_undo_transactions_per_actor: server
                                .inner
                                .config
                                .limits
                                .max_undo_transactions_per_actor,
                            undo_stack,
                            redo_stack,
                            session_data,
                        }).await.is_err() { break; }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            event = closed_rx.recv() => {
                if event.is_ok() {
                    let _ = send_server(
                        &mut sender,
                        &ServerMessage::SessionExpired {
                            message: "the file was removed or restored".into(),
                        },
                    ).await;
                    break;
                }
            }
            event = session_data_rx.recv() => {
                match event {
                    // Never echo this actor; strip only positions from the same identity's other sockets.
                    Ok(event)
                        if event.connection_id != connection_id
                            && event.actor != actor =>
                    {
                        let event_data = if event.identity.id == identity.id {
                            event.data.map(without_live_position)
                        } else {
                            event.data
                        };
                        let message = match event_data {
                            Some(data) => ServerMessage::SessionDataUpdate {
                                actor: event.actor,
                                identity: event.identity,
                                data,
                            },
                            None => ServerMessage::SessionDataLeft {
                                actor: event.actor,
                                identity: event.identity,
                            },
                        };
                        if send_server(&mut sender, &message).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        for peer in session_data_snapshot(&document_room, actor, &identity.id).await {
                            let message = ServerMessage::SessionDataUpdate {
                                actor: peer.actor,
                                identity: peer.identity,
                                data: peer.data,
                            };
                            if send_server(&mut sender, &message).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    let removed = document_room
        .session_data
        .lock()
        .await
        .remove(&connection_id);
    if let Some(peer) = removed {
        document_room.release_memory(peer.estimated_memory_bytes);
    }
    let _ = document_room.session_data_tx.send(SessionDataEvent {
        connection_id,
        actor,
        identity,
        data: None,
    });
    server.leave_session(&document_id, &document_room).await;
}

async fn send_server(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    message: &ServerMessage,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(message).expect("server message is serializable");
    sender.send(Message::Text(json.into())).await
}
