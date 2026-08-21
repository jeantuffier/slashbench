use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use chrono::{DateTime, Utc};
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

#[derive(Deserialize)]
struct ListQuery {
    page: Option<i64>,
    limit: Option<i64>,
}

fn internal_error(e: sqlx::Error) -> HttpResponse {
    HttpResponse::InternalServerError().json(ErrorBody { error: e.to_string() })
}

#[post("/items")]
async fn create_item(db: web::Data<PgPool>, new_item: web::Json<NewItem>) -> impl Responder {
    let result = sqlx::query_as::<_, Item>(
        "INSERT INTO items (name, description, price_cents, quantity)
         VALUES ($1, $2, $3, $4)
         RETURNING id, name, description, price_cents, quantity, created_at, updated_at",
    )
    .bind(&new_item.name)
    .bind(&new_item.description)
    .bind(new_item.price_cents)
    .bind(new_item.quantity)
    .fetch_one(db.get_ref())
    .await;

    match result {
        Ok(item) => HttpResponse::Created().json(item),
        Err(e) => internal_error(e),
    }
}

#[get("/items/{id}")]
async fn get_item(db: web::Data<PgPool>, path: web::Path<i64>) -> impl Responder {
    let id = path.into_inner();
    let result = sqlx::query_as::<_, Item>(
        "SELECT id, name, description, price_cents, quantity, created_at, updated_at
         FROM items WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db.get_ref())
    .await;

    match result {
        Ok(Some(item)) => HttpResponse::Ok().json(item),
        Ok(None) => HttpResponse::NotFound().json(ErrorBody { error: "not found".into() }),
        Err(e) => internal_error(e),
    }
}

#[get("/items")]
async fn list_items(db: web::Data<PgPool>, query: web::Query<ListQuery>) -> impl Responder {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = (page - 1) * limit;

    let items = match sqlx::query_as::<_, Item>(
        "SELECT id, name, description, price_cents, quantity, created_at, updated_at
         FROM items ORDER BY id LIMIT $1 OFFSET $2",
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(db.get_ref())
    .await
    {
        Ok(items) => items,
        Err(e) => return internal_error(e),
    };

    let total: i64 = match sqlx::query_scalar("SELECT reltuples::bigint FROM pg_class WHERE oid = 'items'::regclass")
        .fetch_one(db.get_ref())
        .await
    {
        Ok(total) => total,
        Err(e) => return internal_error(e),
    };

    HttpResponse::Ok().json(ItemList { items, page, limit, total })
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    // Explicit, matched across all six stacks — see rocket/src/main.rs for why.
    let pool: PgPool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(pool.clone()))
            .service(create_item)
            .service(get_item)
            .service(list_items)
    })
    .keep_alive(std::time::Duration::from_secs(75))
    .bind(("0.0.0.0", 8080))?
    .run()
    .await
}
