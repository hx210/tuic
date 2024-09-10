use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use axum::{
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use lateinit::LateInit;
use uuid::Uuid;

pub static ONLINE: LateInit<HashMap<Uuid, AtomicU64>> = LateInit::new();

pub async fn start(addr: SocketAddr, users: Arc<HashMap<Uuid, Box<[u8]>>>) {
    let mut online = HashMap::new();
    for (user, _) in users.iter() {
        online.insert(user.to_owned(), AtomicU64::new(0));
    }
    unsafe { ONLINE.init(online) };
    let app = Router::new()
        .route("/kick", post(kick))
        .route("/online", get(list_online));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    log::warn!("restful server started, listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn kick(Json(_users): Json<Vec<Uuid>>) -> StatusCode {
    StatusCode::OK
}

async fn list_online() -> (StatusCode, Json<HashMap<Uuid, u64>>) {
    let mut result = HashMap::new();
    for (user, count) in ONLINE.iter() {
        let count = count.load(Ordering::Relaxed);
        if count != 0 {
            result.insert(user.to_owned(), count);
        }
    }

    (StatusCode::OK, Json(result))
}
