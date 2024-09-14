use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
};

use axum::{
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use axum_extra::{
    headers::{authorization::Bearer, Authorization},
    TypedHeader,
};
use lateinit::LateInit;
use tracing::warn;
use uuid::Uuid;

use crate::CONFIG;

static ONLINE: LateInit<HashMap<Uuid, AtomicU64>> = LateInit::new();

pub async fn start() {
    let mut online = HashMap::new();
    for (user, _) in CONFIG.users.iter() {
        online.insert(user.to_owned(), AtomicU64::new(0));
    }
    unsafe { ONLINE.init(online) };
    let restful = CONFIG.restful.as_ref().unwrap();
    let addr = restful.addr;
    let app = Router::new()
        .route("/kick", post(kick))
        .route("/online", get(list_online));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    warn!("RESTful server started, listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}

async fn kick(
    TypedHeader(token): TypedHeader<Authorization<Bearer>>,
    Json(_users): Json<Vec<Uuid>>,
) -> StatusCode {
    if let Some(restful) = &CONFIG.restful
        && restful.secret != token.token()
    {
        return StatusCode::UNAUTHORIZED;
    }
    StatusCode::OK
}

async fn list_online(
    TypedHeader(token): TypedHeader<Authorization<Bearer>>,
) -> (StatusCode, Json<HashMap<Uuid, u64>>) {
    if let Some(restful) = &CONFIG.restful
        && restful.secret != token.token()
    {
        return (StatusCode::UNAUTHORIZED, Json(HashMap::new()));
    }
    let mut result = HashMap::new();
    for (user, count) in ONLINE.iter() {
        let count = count.load(Ordering::Relaxed);
        if count != 0 {
            result.insert(user.to_owned(), count);
        }
    }

    (StatusCode::OK, Json(result))
}

pub fn client_connect(uuid: &Uuid) {
    if CONFIG.restful.is_none() {
        return;
    }
    ONLINE
        .get(uuid)
        .expect("Authorized UUID not present in users table")
        .fetch_add(1, Ordering::Release);
}
pub fn client_disconnect(uuid: &Uuid) {
    if CONFIG.restful.is_none() {
        return;
    }
    ONLINE
        .get(uuid)
        .expect("Authorized UUID not present in users table")
        .fetch_sub(1, Ordering::Release);
}
