use axum::{
    extract::{Path, State}, // ADDED: Path to read the room_id from the URL
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Extension,
    Router,
};
use sqlx::{sqlite::SqlitePoolOptions, Row, SqlitePool}; // THE FIX: Added Row
use std::collections::{HashMap, HashSet}; // ADDED: HashMap for our Telephone Exchange
use std::sync::Arc; // ADDED: For sharing memory safely
use tokio::sync::Mutex;
// Replace your existing tower_http import with this:
use tower_http::{
    trace::TraceLayer,
    set_header::SetResponseHeaderLayer,
};
use axum::http::header;

use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

// --- THE TELEPHONE EXCHANGE ---
// This stores all active chat rooms. Key = room_id, Value = The Radio Tower for that room.
type ExchangeState = Arc<Mutex<HashMap<String, broadcast::Sender<String>>>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedMessage {
    pub sender_id: String,
    pub timestamp: u64,
    pub ciphertext: Vec<u8>,
    pub signature: String,
}

pub trait ChatRoomInterface {
    fn join_room(&mut self, user_id: &str, room_id: &str) -> Result<(), RoomError>;
    fn broadcast_message(&self, room_id: &str, payload: EncryptedMessage) -> Result<(), RoomError>;
    fn validate_and_receive(&self, payload: &EncryptedMessage) -> Result<bool, RoomError>;
    fn leave_room(&mut self, user_id: &str, room_id: &str) -> Result<(), RoomError>;
    fn get_active_peers(&self, room_id: &str) -> HashSet<String>;
}

#[derive(Debug)]
pub enum RoomError {
    RoomFull,
    UserBanned,
    InvalidSignature,
    RateLimitExceeded,
    ConnectionDropped,
}

pub struct SecureRoomManager;

impl ChatRoomInterface for SecureRoomManager {
    fn validate_and_receive(&self, payload: &EncryptedMessage) -> Result<bool, RoomError> {
        if payload.sender_id.trim().is_empty() {
            tracing::warn!("Blocked attempt with blank sender_id");
            return Err(RoomError::InvalidSignature);
        }
        Ok(true)
    }

    fn join_room(&mut self, _user_id: &str, _room_id: &str) -> Result<(), RoomError> {
        Ok(())
    }
    fn broadcast_message(
        &self,
        _room_id: &str,
        _payload: EncryptedMessage,
    ) -> Result<(), RoomError> {
        Ok(())
    }
    fn leave_room(&mut self, _user_id: &str, _room_id: &str) -> Result<(), RoomError> {
        Ok(())
    }
    fn get_active_peers(&self, _room_id: &str) -> HashSet<String> {
        HashSet::new()
    }
}

#[derive(Deserialize)]
struct OtpRequest {
    user_hash: String,
    email: String,
}
#[derive(Deserialize)]
struct VerifyOtpRequest {
    user_hash: String,
    otp_code: String,
}
#[derive(Deserialize)]
struct MobileOtpRequest {
    user_hash: String,
    mobile: String,
}

async fn home_render() -> &'static str {
    "Public SEO: Home Page"
}
async fn news_render() -> &'static str {
    "Public SEO: News & Blog"
}
async fn tube_render() -> &'static str {
    "Public SEO: Govind Tube"
}
async fn community_render() -> &'static str {
    "Public SEO: Community (Signed Code)"
}
async fn upload_video() -> &'static str {
    "Secure: Encrypted Video Uploaded"
}

async fn verify_handshake(State(pool): State<SqlitePool>, body: String) -> &'static str {
    let _ = sqlx::query("INSERT OR IGNORE INTO users (user_hash, tier) VALUES (?, 0)")
        .bind(&body)
        .execute(&pool)
        .await;
    "Tier Upgrade Acknowledged"
}

async fn request_otp(
    State(pool): State<SqlitePool>,
    axum::extract::Json(payload): axum::extract::Json<OtpRequest>,
) -> impl IntoResponse {
    let otp_code: u32 = rand::thread_rng().gen_range(100000..999999);
    let _ = sqlx::query("INSERT OR IGNORE INTO users (user_hash, tier) VALUES (?, 0)")
        .bind(&payload.user_hash)
        .execute(&pool)
        .await;
    let result = sqlx::query("UPDATE users SET email = ?, otp_code = ? WHERE user_hash = ?")
        .bind(&payload.email)
        .bind(&otp_code.to_string())
        .bind(&payload.user_hash)
        .execute(&pool)
        .await;

    match result {
        Ok(_) => {
            tracing::info!(
                "📧 SECURE EMAIL DISPATCH INITIATED TO: {} | CODE: {}",
                payload.email,
                otp_code
            );
            (axum::http::StatusCode::OK, "OTP Generated successfully").into_response()
        }
        Err(_) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "System Failure",
        )
            .into_response(),
    }
}

async fn verify_otp(
    State(pool): State<SqlitePool>,
    axum::extract::Json(payload): axum::extract::Json<VerifyOtpRequest>,
) -> impl IntoResponse {
    let result: Result<(String,), _> =
        sqlx::query_as("SELECT otp_code FROM users WHERE user_hash = ?")
            .bind(&payload.user_hash)
            .fetch_one(&pool)
            .await;
    if let Ok((saved_otp,)) = result {
        if saved_otp == payload.otp_code {
            let _ = sqlx::query("UPDATE users SET tier = 1 WHERE user_hash = ?")
                .bind(&payload.user_hash)
                .execute(&pool)
                .await;
            return (axum::http::StatusCode::OK, "Verified").into_response();
        }
    }
    (axum::http::StatusCode::UNAUTHORIZED, "Invalid OTP").into_response()
}

async fn request_mobile_otp(
    State(pool): State<SqlitePool>,
    axum::extract::Json(payload): axum::extract::Json<MobileOtpRequest>,
) -> impl IntoResponse {
    let mobile_code: u32 = rand::thread_rng().gen_range(100000..999999);
    let _ = sqlx::query("UPDATE users SET otp_code = ? WHERE user_hash = ?")
        .bind(&mobile_code.to_string())
        .bind(&payload.user_hash)
        .execute(&pool)
        .await;
    tracing::info!(
        "📱 SECURE SMS DISPATCH TO: {} | CODE: {}",
        payload.mobile,
        mobile_code
    );
    (axum::http::StatusCode::OK, mobile_code.to_string()).into_response()
}

async fn verify_mobile_otp(
    State(pool): State<SqlitePool>,
    axum::extract::Json(payload): axum::extract::Json<VerifyOtpRequest>,
) -> impl IntoResponse {
    let result: Result<(String,), _> =
        sqlx::query_as("SELECT otp_code FROM users WHERE user_hash = ?")
            .bind(&payload.user_hash)
            .fetch_one(&pool)
            .await;
    if let Ok((saved_otp,)) = result {
        if saved_otp == payload.otp_code {
            let _ = sqlx::query("UPDATE users SET tier = 2 WHERE user_hash = ?")
                .bind(&payload.user_hash)
                .execute(&pool)
                .await;
            return (axum::http::StatusCode::OK, "Mobile Verified").into_response();
        }
    }
    (axum::http::StatusCode::UNAUTHORIZED, "Invalid Mobile OTP").into_response()
}

// --- SECURE CHAT ENGINE (Multi-Room) ---
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};

async fn private_chat_ws(
    ws: WebSocketUpgrade,
    Path(room_id): Path<String>,
    Extension(exchange): Extension<ExchangeState>,
    State(pool): State<SqlitePool>, 
) -> impl IntoResponse {
    tracing::info!("🔌 E2EE Connection Attempted for Room: {}", room_id);
    
    // THE FIX: The 5MB Video limits are now merged cleanly into your active route!
    ws.max_message_size(1024 * 1024 * 5) 
      .max_frame_size(1024 * 1024 * 5)   
      .on_upgrade(move |socket| handle_socket(socket, room_id, exchange, pool))
}

async fn handle_socket(
    mut socket: WebSocket,
    room_id: String,
    exchange: ExchangeState,
    pool: SqlitePool,
) {
    let tx = {
        let mut rooms = exchange.lock().await;
        rooms
            .entry(room_id.clone())
            .or_insert_with(|| {
                tracing::info!("🏗️ Building new isolated Radio Tower for Room: {}", room_id);
                // Keep this as String to match your ExchangeState type definition
                let (tx, _rx) = broadcast::channel::<String>(100);
                tx
            })
            .clone()
    };

    let mut rx = tx.subscribe();
    let room_manager = SecureRoomManager;

    tracing::info!("✅ User tuned into Room: {}", room_id);

    if let Ok(records) = sqlx::query("SELECT payload FROM offline_messages WHERE room_id = ?")
        .bind(&room_id)
        .fetch_all(&pool)
        .await
    {
        let count = records.len();
        if count > 0 {
            tracing::info!("📬 Delivering {} queued offline messages to Room: {}", count, room_id);
            for row in records {
                let payload: String = row.get("payload");
                // Convert database String into Axum's Utf8Bytes
                let _ = socket.send(Message::Text(payload.into())).await;
            }
            let _ = sqlx::query("DELETE FROM offline_messages WHERE room_id = ?")
                .bind(&room_id)
                .execute(&pool)
                .await;
        }
    }

    // 3. The Live Network Loop    
    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if text.len() > 1024 * 1024 * 5 { 
                            tracing::warn!("⚠️ Connection dropped: Message payload exceeded 5MB limit.");
                            break; 
                        }
                        // Utf8Bytes implements Deref<Target = str>, allowing direct validation
                        if let Ok(payload) = serde_json::from_str::<EncryptedMessage>(&text) {
                            match room_manager.validate_and_receive(&payload) {
                                Ok(true) => {
                                    // Convert Utf8Bytes to String for the broadcast channel
                                    let text_string = text.to_string();
                                    let _ = tx.send(text_string.clone());

                                    if tx.receiver_count() < 2 {
                                        tracing::info!("📭 Partner offline. Queuing E2EE message in vault...");
                                        let _ = sqlx::query("INSERT INTO offline_messages (room_id, payload) VALUES (?, ?)")
                                            .bind(&room_id)
                                            .bind(&text_string) // Bind standard String to SQLx
                                            .execute(&pool)
                                            .await;
                                    }
                                }
                                Ok(false) => { tracing::warn!("⚠️ Message dropped by rules."); }
                                Err(e) => { tracing::warn!("⛔ Blocked malicious payload: {:?}", e); }
                            }
                        }
                    }
                    Some(Err(_)) | None => { break; } 
                    _ => {}
                }
            }

            Ok(msg) = rx.recv() => {
                // msg is a String from the broadcast channel; convert to Utf8Bytes for Axum 0.8
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
        }
    }
    tracing::info!("❌ WebSocket Disconnected from Room: {}", room_id);
}

// --- MAIN SERVER ENGINE ---
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    tracing::info!("Starting SQLite Database Engine...");

    let db_url = "sqlite://govind_vault.db?mode=rwc";
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(db_url)
        .await
        .expect("Failed DB Connect");

    sqlx::query("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY AUTOINCREMENT, user_hash TEXT UNIQUE NOT NULL, tier INTEGER NOT NULL DEFAULT 0, email TEXT UNIQUE, otp_code TEXT)").execute(&pool).await.unwrap();
    sqlx::query("CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY AUTOINCREMENT, user_hash TEXT UNIQUE NOT NULL, tier INTEGER NOT NULL DEFAULT 0, email TEXT UNIQUE, otp_code TEXT)").execute(&pool).await.unwrap();

    // THE FIX: Create the Offline Message Vault
    sqlx::query("CREATE TABLE IF NOT EXISTS offline_messages (id INTEGER PRIMARY KEY AUTOINCREMENT, room_id TEXT NOT NULL, payload TEXT NOT NULL)").execute(&pool).await.unwrap();

    // THE FIX: Boot up the global Telephone Exchange
    let exchange: ExchangeState = Arc::new(Mutex::new(HashMap::new()));

    let public_routes = Router::new()
        .route("/", get(home_render))
        .route("/news", get(news_render))
        .route("/tube", get(tube_render))
        .route("/community", get(community_render));

    let protected_routes = Router::new()
        .route("/api/upload_tube", post(upload_video))
        // THE FIX: Allow dynamic room IDs in the URL!
        .route("/api/chat/{room_id}", get(private_chat_ws))
        .route("/api/verify", post(verify_handshake))
        .route("/api/request-otp", post(request_otp))
        .route("/api/verify-otp", post(verify_otp))
        .route("/api/request-mobile-otp", post(request_mobile_otp))
        .route("/api/verify-mobile-otp", post(verify_mobile_otp))
        .layer(Extension(exchange)) // Inject the Exchange into the API
        .route_layer(middleware::from_fn(tier_zero_gatekeeper))
        .with_state(pool.clone());

    let app = Router::new()
        .merge(public_routes)
        .nest("/secure", protected_routes)
        .layer(TraceLayer::new_for_http())
        .layer(tower_http::cors::CorsLayer::permissive())
        
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
        ))
        
        // 👇 PASTE THE HTTP SECURITY HEADERS HERE 👇
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            header::HeaderValue::from_static(
                "default-src 'self'; script-src 'self' 'unsafe-eval' 'wasm-unsafe-eval' blob:; connect-src 'self' ws: wss: stun.l.google.com:19302; style-src 'self' 'unsafe-inline';",
            ),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            header::HeaderValue::from_static("DENY"),
        ));
        
    tracing::info!("Zero-Knowledge Hub listening on 127.0.0.1:3000");
    axum::serve(
        tokio::net::TcpListener::bind("127.0.0.1:3000")
            .await
            .unwrap(),
        app,
    )
    .await
    .unwrap();
}

async fn tier_zero_gatekeeper(
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    let method = req.method().clone();
    if method == axum::http::Method::PUT || method == axum::http::Method::DELETE {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            "Tier 0 Access: Read-Only.",
        )
            .into_response();
    }
    next.run(req).await
}

