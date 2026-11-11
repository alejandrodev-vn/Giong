use axum::{extract::Query, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use std::hint::black_box;

#[derive(Serialize)]
struct Msg { message: &'static str }

#[derive(Serialize)]
struct User { id: u32, name: String, email: String, active: bool, score: f64 }

#[derive(Deserialize)]
struct N { n: Option<u32> }

fn fib(n: u32) -> u64 { if n < 2 { n as u64 } else { fib(n - 1) + fib(n - 2) } }

async fn json() -> Json<Msg> { Json(Msg { message: "Hello, World!" }) }

async fn users() -> Json<Vec<User>> {
    Json((0..100).map(|i| User {
        id: i, name: format!("User {i}"), email: format!("user{i}@example.com"),
        active: i % 2 == 0, score: i as f64 * 1.5,
    }).collect())
}

async fn compute(Query(q): Query<N>) -> Json<serde_json::Value> {
    let n = black_box(q.n.unwrap_or(25));
    Json(serde_json::json!({ "result": fib(n) }))
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/json", get(json))
        .route("/users", get(users))
        .route("/compute", get(compute));
    let l = tokio::net::TcpListener::bind("127.0.0.1:3003").await.unwrap();
    axum::serve(l, app).await.unwrap();
}
