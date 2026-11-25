use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};

tonic::include_proto!("_");

use scrcpy_service_client::ScrcpyServiceClient;

pub struct StreamRelay {
    grpc_url: String,
    pub tx: broadcast::Sender<Bytes>,
    client: Arc<Mutex<Option<ScrcpyServiceClient<tonic::transport::Channel>>>>,
    cached_sps: Arc<Mutex<Option<Bytes>>>,
    cached_pps: Arc<Mutex<Option<Bytes>>>,
    gop_frames: Arc<Mutex<Vec<Bytes>>>,
}

impl StreamRelay {
    pub fn new(grpc_url: String) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            grpc_url,
            tx,
            client: Arc::new(Mutex::new(None)),
            cached_sps: Arc::new(Mutex::new(None)),
            cached_pps: Arc::new(Mutex::new(None)),
            gop_frames: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn request_idr(&self) -> Result<(), String> {
        let mut client_opt = self.client.lock().await;
        if let Some(ref mut c) = *client_opt {
            match c.on_request_idr_frame(Empty {}).await {
                Ok(res) => {
                    info!("Requested IDR keyframe, response: {:?}", res.into_inner());
                    Ok(())
                }
                Err(e) => {
                    warn!("Failed to request IDR: {}", e);
                    Err(e.to_string())
                }
            }
        } else {
            Err("gRPC client not connected".to_string())
        }
    }

    pub async fn get_initial_packets(&self) -> Vec<Bytes> {
        let mut packets = Vec::new();
        let sps_guard = self.cached_sps.lock().await;
        if let Some(ref sps) = *sps_guard {
            packets.push(sps.clone());
        }
        let pps_guard = self.cached_pps.lock().await;
        if let Some(ref pps) = *pps_guard {
            if sps_guard.as_ref() != Some(pps) {
                packets.push(pps.clone());
            }
        }
        let gop = self.gop_frames.lock().await;
        for frame in gop.iter() {
            packets.push(frame.clone());
        }
        packets
    }

    pub async fn run_loop(self: Arc<Self>) {
        loop {
            info!("Connecting to gRPC video service at {}...", self.grpc_url);
            match ScrcpyServiceClient::connect(self.grpc_url.clone()).await {
                Ok(mut client) => {
                    info!("Connected to ScrcpyService! Starting video stream...");
                    
                    {
                        let mut c_guard = self.client.lock().await;
                        *c_guard = Some(client.clone());
                    }

                    match client.on_start(Empty {}).await {
                        Ok(res) => {
                            let mut stream = res.into_inner();
                            info!("Video stream active! Relaying frames...");

                            // Request an initial IDR frame immediately
                            let _ = client.on_request_idr_frame(Empty {}).await;

                            let mut frame_count: u64 = 0;
                            let mut total_bytes: u64 = 0;
                            let mut last_log = tokio::time::Instant::now();

                            while let Ok(Some(msg)) = stream.message().await {
                                let mut frame_bytes = None;
                                for (_k, v) in msg.payload {
                                    if let Some(param_value::Value::ValBytes(b)) = v.value {
                                        frame_bytes = Some(b);
                                        break;
                                    }
                                }

                                if let Some(buf) = frame_bytes {
                                    if buf.len() < 4 {
                                        continue;
                                    }

                                    frame_count += 1;
                                    total_bytes += buf.len() as u64;

                                    let nal_types = extract_nal_types(&buf);
                                    let b = Bytes::from(buf);

                                    for &t in &nal_types {
                                        if t == 7 {
                                            *self.cached_sps.lock().await = Some(b.clone());
                                        } else if t == 8 {
                                            *self.cached_pps.lock().await = Some(b.clone());
                                        } else if t == 5 {
                                            let mut gop = self.gop_frames.lock().await;
                                            gop.clear();
                                            gop.push(b.clone());
                                        } else if t == 1 {
                                            let mut gop = self.gop_frames.lock().await;
                                            if !gop.is_empty() && gop.len() < 60 {
                                                gop.push(b.clone());
                                            }
                                        }
                                    }

                                    let _ = self.tx.send(b);

                                    if last_log.elapsed() >= tokio::time::Duration::from_secs(3) {
                                        info!(
                                            "Stream active: total {} frames, {} KB relayed",
                                            frame_count,
                                            total_bytes / 1024
                                        );
                                        last_log = tokio::time::Instant::now();
                                    }
                                }
                            }
                            warn!("Video stream ended from device server.");
                        }
                        Err(e) => {
                            error!("onStart call failed: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("Connection to ScrcpyService failed: {}. Retrying in 2s...", e);
                }
            }

            {
                let mut c_guard = self.client.lock().await;
                *c_guard = None;
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }
    }
}

fn extract_nal_types(data: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let len = data.len();
    let mut i = 0;
    while i + 4 <= len {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                let nal = data[i + 3] & 0x1F;
                types.push(nal);
                i += 3;
            } else if data[i + 2] == 0 && data[i + 3] == 1 && i + 4 < len {
                let nal = data[i + 4] & 0x1F;
                types.push(nal);
                i += 4;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    types
}
