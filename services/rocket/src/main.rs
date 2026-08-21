#[macro_use]
extern crate rocket;

use chrono::{DateTime, Utc};
use rocket::http::Status;
use rocket::response::status;
use rocket::serde::json::Json;
use rocket::State;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

#[derive(Serialize, sqlx::FromRow)]
struct Item {
    id: i64,
    name: String,
    description: Option<String>,
    price_cents: i32,
    quantity: i32,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
struct NewItem {
    name: String,
    description: Option<String>,
    price_cents: i32,
    quantity: i32,
}

#[derive(Serialize)]
struct ItemList {
    items: Vec<Item>,
    page: i64,
    limit: i64,
    total: i64,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

type ApiError = status::Custom<Json<ErrorBody>>;

fn internal_error(e: sqlx::Error) -> ApiError {
    status::Custom(Status::InternalServerError, Json(ErrorBody { error: e.to_string() }))
}

#[post("/items", data = "<new_item>")]
async fn create_item(
    db: &State<PgPool>,
    new_item: Json<NewItem>,
) -> Result<status::Created<Json<Item>>, ApiError> {
    let item = sqlx::query_as::<_, Item>(
        "INSERT INTO items (name, description, price_cents, quantity)
         VALUES ($1, $2, $3, $4)
         RETURNING id, name, description, price_cents, quantity, created_at, updated_at",
    )
    .bind(&new_item.name)
    .bind(&new_item.description)
    .bind(new_item.price_cents)
    .bind(new_item.quantity)
    .fetch_one(db.inner())
    .await
    .map_err(internal_error)?;

    Ok(status::Created::new("/items").body(Json(item)))
}

#[get("/items/<id>")]
async fn get_item(db: &State<PgPool>, id: i64) -> Result<Json<Item>, ApiError> {
    let item = sqlx::query_as::<_, Item>(
        "SELECT id, name, description, price_cents, quantity, created_at, updated_at
         FROM items WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.inner())
    .await
    .map_err(internal_error)?;

    match item {
        Some(item) => Ok(Json(item)),
        None => Err(status::Custom(
            Status::NotFound,
            Json(ErrorBody { error: "not found".into() }),
        )),
    }
}

#[get("/items?<page>&<limit>")]
async fn list_items(
    db: &State<PgPool>,
    page: Option<i64>,
    limit: Option<i64>,
) -> Result<Json<ItemList>, ApiError> {
    let page = page.unwrap_or(1).max(1);
    let limit = limit.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * limit;

    let items = sqlx::query_as::<_, Item>(
        "SELECT id, name, description, price_cents, quantity, created_at, updated_at
         FROM items ORDER BY id LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(db.inner())
    .await
    .map_err(internal_error)?;

    let total: i64 = sqlx::query_scalar("SELECT reltuples::bigint FROM pg_class WHERE oid = 'items'::regclass")
        .fetch_one(db.inner())
        .await
        .map_err(internal_error)?;

    Ok(Json(ItemList { items, page, limit, total }))
}

#[launch]
async fn rocket() -> _ {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    // Explicit, matched across all six stacks (CLAUDE.md's "raw drivers,
    // matched" fairness principle extends to pool size too — left implicit,
    // this was a confound: sqlx/HikariCP/postgres.js all nominally default
    // to 10, but observed behavior under load diverged sharply anyway).
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    rocket::build()
        .manage(pool)
        .mount("/", routes![create_item, get_item, list_items])
}
