use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::header,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::device::{DeviceManager, InputCommand};
use crate::stream::StreamRelay;

#[derive(Clone)]
pub struct AppState {
    pub device: Arc<DeviceManager>,
    pub relay: Arc<StreamRelay>,
}

fn default_sens() -> f32 {
    1.0
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum WsControlMessage {
    #[serde(rename = "click")]
    Click { x: u32, y: u32 },
    #[serde(rename = "touch")]
    Touch { action: String, x: u32, y: u32 },
    #[serde(rename = "wheel")]
    Wheel {
        x: u32,
        y: u32,
        #[serde(rename = "deltaY")]
        delta_y: i32,
        #[serde(default = "default_sens")]
        sensitivity: f32,
    },
    #[serde(rename = "swipe")]
    Swipe {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        #[serde(default)]
        velocity: u32,
        #[serde(default)]
        duration: u32,
    },
    #[serde(rename = "fling")]
    Fling {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        #[serde(default)]
        velocity: u32,
    },
    #[serde(rename = "key")]
    Key { key: String },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "requestIdr")]
    RequestIdr,
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Serialize)]
pub struct DeviceInfo {
    pub target: String,
    pub width: u32,
    pub height: u32,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/jmuxer.min.js", get(jmuxer_handler))
        .route("/ws", get(ws_handler))
        .route("/api/device", get(device_info_handler))
        .route("/api/idr", post(idr_handler))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

async fn jmuxer_handler() -> Response {
    ([(header::CONTENT_TYPE, "application/javascript")], include_str!("../web/jmuxer.min.js")).into_response()
}

async fn device_info_handler(State(state): State<AppState>) -> Json<DeviceInfo> {
    Json(DeviceInfo {
        target: state.device.target.clone(),
        width: state.device.width,
        height: state.device.height,
    })
}

async fn idr_handler(State(state): State<AppState>) -> &'static str {
    let _ = state.relay.request_idr().await;
    "ok"
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    info!("New WebSocket client connected!");
    let (mut sender, mut receiver) = socket.split();

    // 1. Subscribe to broadcast channel FIRST so no live frames are missed
    let mut rx = state.relay.tx.subscribe();

    // 2. Send cached SPS/PPS and GOP frames
    let initial_packets = state.relay.get_initial_packets().await;
    info!("Sending {} initial GOP packets to new client", initial_packets.len());
    for packet in initial_packets {
        if sender.send(Message::Binary(packet.to_vec())).await.is_err() {
            return;
        }
    }

    // 3. Request fresh IDR from phone hardware encoder
    let _ = state.relay.request_idr().await;

    // 4. Forward live stream to client
    let send_task = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if sender.send(Message::Binary(msg.to_vec())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("WebSocket client lagged by {} frames; catching up", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    // 5. Handle client control commands
    let device = state.device.clone();
    let relay = state.relay.clone();
    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(cmd) = serde_json::from_str::<WsControlMessage>(&text) {
                        let dev = device.clone();
                        let rel = relay.clone();
                        match cmd {
                            WsControlMessage::Click { x, y } => {
                                dev.send_input(InputCommand::Click { x, y });
                            }
                            WsControlMessage::Touch { action, x, y } => match action.as_str() {
                                "down" => dev.send_input(InputCommand::TouchDown { x, y }),
                                "move" => dev.send_input(InputCommand::TouchMove { x, y }),
                                "up" => dev.send_input(InputCommand::TouchUp { x, y }),
                                _ => {}
                            },
                            WsControlMessage::Wheel {
                                x,
                                y,
                                delta_y,
                                sensitivity,
                            } => {
                                dev.send_input(InputCommand::Wheel {
                                    x,
                                    y,
                                    delta_y,
                                    sensitivity,
                                });
                            }
                            WsControlMessage::Swipe {
                                x1,
                                y1,
                                x2,
                                y2,
                                velocity,
                                duration,
                            } => {
                                let vel = if velocity > 0 {
                                    velocity
                                } else if duration > 0 {
                                    let dx = (x2 as i32 - x1 as i32).abs();
                                    let dy = (y2 as i32 - y1 as i32).abs();
                                    let dist = ((dx * dx + dy * dy) as f32).sqrt() as u32;
                                    dist * 1000 / duration.max(50)
                                } else {
                                    2500
                                };
                                dev.send_input(InputCommand::Swipe {
                                    x1,
                                    y1,
                                    x2,
                                    y2,
                                    velocity: vel,
                                });
                            }
                            WsControlMessage::Fling {
                                x1,
                                y1,
                                x2,
                                y2,
                                velocity,
                            } => {
                                dev.send_input(InputCommand::Swipe {
                                    x1,
                                    y1,
                                    x2,
                                    y2,
                                    velocity,
                                });
                            }
                            WsControlMessage::Key { key } => {
                                dev.send_input(InputCommand::Key(key.to_lowercase()));
                            }
                            WsControlMessage::Text { text } => {
                                dev.send_input(InputCommand::Text(text));
                            }
                            WsControlMessage::RequestIdr => {
                                tokio::spawn(async move {
                                    let _ = rel.request_idr().await;
                                });
                            }
                            WsControlMessage::Ping => {}
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }
    info!("WebSocket client disconnected.");
}
