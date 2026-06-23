//! atlas-realtime — Realtime Gateway WebSocket (doc 40).
//!
//! M1 : fan-out **mono-nœud** via `tokio::broadcast` (le pont NATS multi-nœuds viendra,
//! doc 40 §6). Les parties pures (protocole, abonnements) sont testées ; le handler WS
//! orchestre lecture client / poussée d'événements / heartbeat.

pub mod auth;
pub mod protocol;
pub mod registry;

use auth::{AuthCtx, Authenticator, DefaultPdp, DevAuthenticator, Pdp};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use protocol::{ClientMsg, ServerMsg};
use registry::Subscriptions;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Événement diffusé aux abonnés (doc 28/40).
#[derive(Debug, Clone)]
pub struct Event {
    pub channel: String,
    pub kind: String,
    pub data: Value,
}

/// Hub de diffusion (clonable). M1 : broadcast en mémoire d'un nœud + auth/PDP.
#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<Event>,
    authenticator: Arc<dyn Authenticator>,
    pdp: Arc<dyn Pdp>,
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(1024);
        Self {
            tx,
            authenticator: Arc::new(DevAuthenticator),
            pdp: Arc::new(DefaultPdp),
        }
    }

    /// Publie un événement à tous les abonnés du canal (filtré côté connexion).
    pub fn publish(&self, channel: impl Into<String>, kind: impl Into<String>, data: Value) {
        // Ignore l'erreur s'il n'y a aucun abonné (pas de récepteur).
        let _ = self.tx.send(Event {
            channel: channel.into(),
            kind: kind.into(),
            data,
        });
    }

    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// Routes temps réel (montées sous `/v1` par le service core).
pub fn routes(hub: Hub) -> Router {
    Router::new().route("/ws", get(ws_upgrade)).with_state(hub)
}

#[derive(Debug, Deserialize)]
struct WsParams {
    #[serde(default)]
    token: String,
}

async fn ws_upgrade(
    State(hub): State<Hub>,
    Query(params): Query<WsParams>,
    ws: WebSocketUpgrade,
) -> Response {
    // Auth du jeton AVANT l'upgrade (doc 40 §4.1). Échec → 401, aucune socket ouverte.
    let ctx = match hub.authenticator.authenticate(&params.token) {
        Some(ctx) => ctx,
        None => return (StatusCode::UNAUTHORIZED, "token requis").into_response(),
    };
    ws.on_upgrade(move |socket| session_loop(socket, hub, ctx))
}

/// Boucle d'une connexion : abonnements (scopés PDP) + poussée d'événements + heartbeat + reprise.
async fn session_loop(mut socket: WebSocket, hub: Hub, ctx: AuthCtx) {
    let mut subs = Subscriptions::new();
    let mut rx = hub.subscribe();
    let mut seq: u64 = 0;
    let mut hb = tokio::time::interval(std::time::Duration::from_secs(20));

    loop {
        tokio::select! {
            // Message du client (subscribe / unsubscribe / ping).
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(txt))) => {
                        if let Some(msg) = ClientMsg::parse(&txt) {
                            if !handle_client(&mut socket, &mut subs, &hub, &ctx, msg).await {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            // Événement diffusé : poussé seulement si la session est abonnée à son canal.
            ev = rx.recv() => {
                match ev {
                    Ok(ev) if subs.should_deliver(&ev.channel) => {
                        seq += 1;
                        let out = ServerMsg::Event { channel: ev.channel, kind: ev.kind, data: ev.data, seq };
                        if send(&mut socket, &out).await.is_err() { break; }
                    }
                    Ok(_) => {} // pas abonné → ignoré (aucune fuite)
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Backpressure : le client a pris du retard → invite à resync.
                        let _ = send(&mut socket, &ServerMsg::Resync { channel: "*".into() }).await;
                    }
                    Err(_) => break,
                }
            }
            // Heartbeat applicatif.
            _ = hb.tick() => {
                if send(&mut socket, &ServerMsg::Pong).await.is_err() { break; }
            }
        }
    }
}

/// Traite un message client ; renvoie false pour fermer la connexion.
async fn handle_client(
    socket: &mut WebSocket,
    subs: &mut Subscriptions,
    hub: &Hub,
    ctx: &AuthCtx,
    msg: ClientMsg,
) -> bool {
    match msg {
        ClientMsg::Subscribe { channels } => {
            for ch in channels {
                // Abonnement scopé par permissions (doc 40 §5) : refus → Denied, pas d'abonnement.
                if !hub.pdp.can_subscribe(ctx, &ch) {
                    let denied = ServerMsg::Denied {
                        channel: ch,
                        reason: "forbidden".into(),
                    };
                    if send(socket, &denied).await.is_err() {
                        return false;
                    }
                    continue;
                }
                subs.subscribe(&ch);
                if send(socket, &ServerMsg::Ack { channel: ch }).await.is_err() {
                    return false;
                }
            }
        }
        ClientMsg::Unsubscribe { channels } => {
            for ch in channels {
                subs.unsubscribe(&ch);
            }
        }
        ClientMsg::Ping => {
            if send(socket, &ServerMsg::Pong).await.is_err() {
                return false;
            }
        }
    }
    true
}

async fn send(socket: &mut WebSocket, msg: &ServerMsg) -> Result<(), axum::Error> {
    let txt = serde_json::to_string(msg).unwrap_or_else(|_| "{}".into());
    socket.send(Message::Text(txt)).await
}
