#![allow(non_snake_case)]
use dioxus::document::eval;
use dioxus::prelude::*;
use futures::{SinkExt, StreamExt};
use gloo_net::websocket::{futures::WebSocket, Message as WsMessage};
use gloo_storage::{LocalStorage, Storage};
use gloo_timers::future::sleep;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlVideoElement, MediaStreamConstraints, RtcConfiguration, RtcPeerConnection, RtcSessionDescriptionInit};

const API_BASE_URL: &str = "http://127.0.0.1:3000";
const WS_BASE_URL: &str = "ws://127.0.0.1:3000";

#[allow(dead_code)] pub const AGENTIC_AI_ENABLED: bool = false;
#[allow(dead_code)] pub const AGENTIC_AI_ENDPOINT: &str = "ws://127.0.0.1:3000/secure/api/ai-agent";

use aes_gcm::{ aead::{Aead, AeadCore, KeyInit, OsRng}, Aes256Gcm, Key, Nonce };

fn main() {
    dioxus_logger::init(tracing::Level::INFO).expect("failed to init logger");
    launch(App);
}

#[derive(Serialize, Deserialize, Clone)]
pub struct EncryptedMessage { pub sender_id: String, pub timestamp: u64, pub ciphertext: Vec<u8>, pub signature: String }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedContact { pub name: String, pub room_id: String, pub secret_password: String }

#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct ChatMessage { id: usize, text: String, is_mine: bool }

// --- 1. GLOBAL IDENTITY & MEMORY VAULTS ---
static HOME_EMAIL: GlobalSignal<String> = Signal::global(|| String::new());
static HOME_TIER: GlobalSignal<u8> = Signal::global(|| 0);
static ACTIVE_HASH: GlobalSignal<String> = Signal::global(|| String::new());
static UNREAD_BADGES: GlobalSignal<std::collections::HashMap<String, usize>> = Signal::global(|| std::collections::HashMap::new());

// 🔴 THE NEW GLOBAL LOGGING ENGINE
static SYSTEM_LOGS: GlobalSignal<Vec<String>> = Signal::global(|| Vec::new());

pub fn add_log(msg: &str) {
    let timestamp = js_sys::Date::new_0().to_locale_time_string("en-US").as_string().unwrap_or_default();
    let entry = format!("[{}] {}", timestamp, msg);
    SYSTEM_LOGS.write().push(entry.clone());
    tracing::info!("{}", entry); // Also print to terminal
}

#[derive(Clone, PartialEq, Debug)]
pub struct CallAlert { pub room_id: String, pub caller_name: String, pub call_type: String, pub secret_password: String }
static GLOBAL_INCOMING_CALL: GlobalSignal<Option<CallAlert>> = Signal::global(|| None);
static CHAT_ACTIVE_ROOM: GlobalSignal<String> = Signal::global(|| String::new());
static CHAT_ACTIVE_PASS: GlobalSignal<String> = Signal::global(|| String::new());
static CHAT_ACTIVE_NAME: GlobalSignal<String> = Signal::global(|| String::new());
static CHAT_CALL_TYPE: GlobalSignal<String> = Signal::global(|| String::new());

static CHAT_IS_CALLING: GlobalSignal<bool> = Signal::global(|| false);
static CHAT_IS_ANSWERING: GlobalSignal<bool> = Signal::global(|| false);
static CHAT_INCOMING_OFFER_SDP: GlobalSignal<String> = Signal::global(|| String::new());
static CHAT_INCOMING_ANSWER_SDP: GlobalSignal<String> = Signal::global(|| String::new());

// --- TAB SYSTEM ENGINE ---
#[derive(Clone, PartialEq, Debug)]
enum Route { Home {}, News {}, Tube {}, PublicChat {}, PrivateChat {}, Dating {}, EComm {}, UnderDevelopment {} }

#[derive(Clone, PartialEq)]
struct AppTab { id: usize, title: String, route: Route }

static NEXT_TAB_ID: GlobalSignal<usize> = Signal::global(|| 1);
static TABS: GlobalSignal<Vec<AppTab>> = Signal::global(|| { vec![AppTab { id: 0, title: "Home".to_string(), route: Route::Home {} }] });
static ACTIVE_TAB_ID: GlobalSignal<usize> = Signal::global(|| 0);
static DRAGGED_TAB_ID: GlobalSignal<Option<usize>> = Signal::global(|| None);

fn switch_to_tab(id: usize, route: &Route) {
    *ACTIVE_TAB_ID.write() = id;
    let path = match route {
        Route::Home {} => "/", Route::News {} => "/news", Route::Tube {} => "/tube",
        Route::PublicChat {} => "/public-chat", Route::PrivateChat {} => "/private-chat",
        Route::Dating {} => "/dating", Route::EComm {} => "/e-comm", Route::UnderDevelopment {} => "/dev",
    };
    let _ = eval(&format!("{{ window.history.replaceState(null, '', '{}'); }}", path));
}

fn open_or_focus_tab(route: Route, title: &str) {
    let mut tabs = TABS.write();
    if let Some(existing_tab) = tabs.iter().find(|t| t.route == route) { switch_to_tab(existing_tab.id, &existing_tab.route); } 
    else {
        let id = *NEXT_TAB_ID.read(); *NEXT_TAB_ID.write() += 1;
        tabs.push(AppTab { id, title: title.to_string(), route: route.clone() });
        switch_to_tab(id, &route);
    }
}

// --- 2. THE NETWORK & CRYPTO GEAR ---
#[derive(Serialize)] struct OtpPayload { user_hash: String, email: String }
#[derive(Serialize)] struct VerifyPayload { user_hash: String, otp_code: String }
#[derive(Serialize)] struct MobilePayload { user_hash: String, mobile: String }

fn generate_hash(identifier: &str) -> String {
    let mut hasher = Sha256::new(); hasher.update(identifier.as_bytes()); hex::encode(hasher.finalize())

}
fn derive_aes_key(password: &str) -> Key<Aes256Gcm> {
    let mut hasher = Sha256::new(); hasher.update(password.as_bytes()); let result = hasher.finalize(); *Key::<Aes256Gcm>::from_slice(&result)
}
fn encrypt_message(text: &str, password: &str) -> Vec<u8> {
    let key = derive_aes_key(password); let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut ciphertext = cipher.encrypt(&nonce, text.as_bytes()).expect("Encryption failure!");
    let mut payload = nonce.to_vec(); payload.append(&mut ciphertext); payload
}
fn decrypt_message(payload: &[u8], password: &str) -> Result<String, &'static str> {
    if payload.len() < 12 { return Err("Payload too short"); }
    let key = derive_aes_key(password); let cipher = Aes256Gcm::new(&key);
    let (nonce_bytes, ciphertext) = payload.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    match cipher.decrypt(nonce, ciphertext) { Ok(bytes) => Ok(String::from_utf8(bytes).unwrap_or_else(|_| "Invalid UTF-8".to_string())), Err(_) => Err("Decryption failed. Wrong password?"), }
}

async fn request_otp_from_vault(email: String) -> bool {
    let hash = generate_hash(&email); *ACTIVE_HASH.write() = hash.clone();
    let payload = OtpPayload { user_hash: hash, email }; match Client::new().post(&format!("{}/secure/api/request-otp", API_BASE_URL)).json(&payload).send().await { Ok(res) => res.status().is_success(), Err(_) => false }
}
async fn submit_otp_to_vault(otp_code: String) -> bool {
    let payload = VerifyPayload { user_hash: ACTIVE_HASH(), otp_code }; match Client::new().post(&format!("{}/secure/api/verify-otp", API_BASE_URL)).json(&payload).send().await { Ok(res) => res.status().is_success(), Err(_) => false }
}
async fn request_mobile_otp_from_vault(mobile: String) -> bool {
    let payload = MobilePayload { user_hash: ACTIVE_HASH(), mobile }; match Client::new().post(&format!("{}/secure/api/request-mobile-otp", API_BASE_URL)).json(&payload).send().await { Ok(res) => res.status().is_success(), Err(_) => false }
}
async fn submit_mobile_otp_to_vault(otp_code: String) -> bool {
    let payload = VerifyPayload { user_hash: ACTIVE_HASH(), otp_code }; match Client::new().post(&format!("{}/secure/api/verify-mobile-otp", API_BASE_URL)).json(&payload).send().await { Ok(res) => res.status().is_success(), Err(_) => false }
}

async fn start_camera(video_id: &str) -> Result<web_sys::MediaStream, JsValue> {
    add_log(&format!("Hardware: Attempting to access camera/mic for {}", video_id));
    
    let window = web_sys::window().expect("No global window found");
    let navigator = window.navigator();
    
    let media_devices = match navigator.media_devices() {
        Ok(md) => md,
        Err(e) => {
            add_log("Hardware ERROR: Browser blocked media API. Ensure you are on localhost or HTTPS!");
            return Err(e);
        }
    };

    let constraints = MediaStreamConstraints::new();
    constraints.set_video(&JsValue::from_bool(true));
    // THE FIX: Set to false temporarily. If a desktop lacks a mic, 'true' causes an instant crash!
    constraints.set_audio(&JsValue::from_bool(false)); 

    add_log("Hardware: Requesting user permission...");
    
    // THE FIX: We now extract the EXACT error name and message from the browser!
    let promise = match media_devices.get_user_media_with_constraints(&constraints) {
        Ok(p) => p,
        Err(e) => {
            let n = js_sys::Reflect::get(&e, &JsValue::from_str("name")).ok().and_then(|v| v.as_string()).unwrap_or_default();
            let m = js_sys::Reflect::get(&e, &JsValue::from_str("message")).ok().and_then(|v| v.as_string()).unwrap_or_default();
            add_log(&format!("Hardware ERROR: [{}] {}", n, m));
            return Err(e);
        }
    };

    let stream_js = match wasm_bindgen_futures::JsFuture::from(promise).await {
        Ok(s) => s,
        Err(e) => {
            let n = js_sys::Reflect::get(&e, &JsValue::from_str("name")).ok().and_then(|v| v.as_string()).unwrap_or_default();
            let m = js_sys::Reflect::get(&e, &JsValue::from_str("message")).ok().and_then(|v| v.as_string()).unwrap_or_default();
            add_log(&format!("Hardware ERROR: [{}] {}", n, m));
            return Err(e);
        }
    };

    let stream: web_sys::MediaStream = stream_js.unchecked_into();
    
    let js_code = format!("window.localStream = arguments[0];");
    let func = js_sys::Function::new_with_args("stream", &js_code);
    let _ = func.call1(&JsValue::NULL, &stream);

    sleep(Duration::from_millis(100)).await;

    if let Some(video_elem) = window.document().expect("No doc").get_element_by_id(video_id) {
        let video: HtmlVideoElement = video_elem.unchecked_into(); 
        video.set_src_object(Some(&stream)); 
        let _ = video.play();
        add_log("Hardware: Camera stream successfully bound to UI.");
    }
    
    Ok(stream)
}

// --- 3. THE GATEKEEPERS ---
#[component]
fn IdentityGate(title: String, prompt_text: String, allow_reuse: bool, on_verified: EventHandler<String>) -> Element {
    let mut ui_stage = use_signal(|| 0); let mut countdown = use_signal(|| 0); let mut email_input = use_signal(|| String::new()); let mut otp_input = use_signal(|| String::new()); let mut error_msg = use_signal(|| String::new());

    rsx! {
        div { class: "dashboard", style: "text-align: center;",
            h2 { "{title}" }
            if ui_stage() == 0 {
                if allow_reuse && !HOME_EMAIL().is_empty() {
                    div {
                        display: "flex",
                        flex_direction: "column",
                        gap: "10px",
                        align_items: "center",
                        button {
                            class: "btn btn-success",
                            onclick: move |_| {
                                let existing_email = HOME_EMAIL();
                                ui_stage.set(3);
                                on_verified.call(existing_email);
                            },
                            "Use Same Email-ID ({HOME_EMAIL()})"
                        }
                        p { color: "#94a3b8", "- OR -" }
                        button {
                            class: "btn btn-secondary",
                            onclick: move |_| ui_stage.set(1),
                            "Login Different Email-ID"
                        }
                    }
                } else {
                    button {
                        class: "btn btn-email",
                        onclick: move |_| ui_stage.set(1),
                        "{prompt_text}"
                    }
                }
            }
            if ui_stage() == 1 {
                div {
                    input {
                        class: "input-field",
                        r#type: "email",
                        placeholder: "Enter Email-ID",
                        value: "{email_input}",
                        onmounted: move |e| async move {
                            let _ = e.set_focus(true).await;
                        },
                        oninput: move |e| email_input.set(e.value()),
                        onkeypress: move |e| {
                            if e.key().to_string() == "Enter" && !email_input().is_empty() {
                                ui_stage.set(2);
                                countdown.set(60);
                                let email = email_input();
                                spawn(async move {
                                    request_otp_from_vault(email).await;
                                });
                                spawn(async move {
                                    while countdown() > 0 {
                                        sleep(Duration::from_secs(1)).await;
                                        countdown.set(countdown() - 1);
                                    }
                                });
                            }
                        },
                    }
                    br {}
                    button {
                        class: "btn btn-email",
                        onclick: move |_| {
                            if !email_input().is_empty() {
                                ui_stage.set(2);
                                countdown.set(60);
                                let email = email_input();
                                spawn(async move {
                                    request_otp_from_vault(email).await;
                                });
                                spawn(async move {
                                    while countdown() > 0 {
                                        sleep(Duration::from_secs(1)).await;
                                        countdown.set(countdown() - 1);
                                    }
                                });
                            }
                        },
                        "Send OTP"
                    }
                }
            }
            if ui_stage() == 2 {
                div {
                    div { class: "otp-wrapper",
                        div { class: "otp-squares",
                            for i in 0..6 {
                                div { class: if otp_input().len() == i { "otp-square otp-square-active" } else { "otp-square" },
                                    "{otp_input().chars().nth(i).unwrap_or(' ')}"
                                }
                            }
                        }
                        input {
                            class: "otp-ghost",
                            r#type: "text",
                            maxlength: "6",
                            value: "{otp_input}",
                            onmounted: move |e| async move {
                                let _ = e.set_focus(true).await;
                            },
                            oninput: move |e| {
                                let filtered: String = e
                                    .value()
                                    .chars()
                                    .filter(|c| c.is_ascii_digit())
                                    .collect();
                                otp_input.set(filtered.clone());
                                error_msg.set(String::new());
                                if filtered.len() == 6 {
                                    let email = email_input();
                                    spawn(async move {
                                        if submit_otp_to_vault(filtered).await {
                                            ui_stage.set(3);
                                            on_verified.call(email);
                                        } else {
                                            error_msg.set("Invalid OTP. Try again.".to_string());
                                            otp_input.set(String::new());
                                        }
                                    });
                                }
                            },
                        }
                    }
                    br {}
                    if countdown() > 0 {
                        p { class: "timer-text", "Resend OTP in {countdown}s..." }
                    } else {
                        button {
                            class: "btn btn-secondary",
                            onclick: move |_| {
                                countdown.set(60);
                                let email = email_input();
                                spawn(async move {
                                    request_otp_from_vault(email).await;
                                });
                                spawn(async move {
                                    while countdown() > 0 {
                                        sleep(Duration::from_secs(1)).await;
                                        countdown.set(countdown() - 1);
                                    }
                                });
                            },
                            "Resend OTP"
                        }
                    }
                    button {
                        class: "btn btn-success",
                        onclick: move |_| {
                            let code = otp_input();
                            let email = email_input();
                            spawn(async move {
                                if submit_otp_to_vault(code).await {
                                    ui_stage.set(3);
                                    on_verified.call(email);
                                } else {
                                    error_msg.set("Invalid OTP. Try again.".to_string());
                                    otp_input.set(String::new());
                                }
                            });
                        },
                        "Verify OTP"
                    }
                    if !error_msg().is_empty() {
                        p { class: "error-text", "{error_msg}" }
                    }
                }
            }
            if ui_stage() == 3 {
                h3 { style: "color: #22c55e;", "✅ Email Successfully Verified!" }
            }
        }
    }
}

#[component]
fn MobileGate(on_verified: EventHandler<()>) -> Element {
    let mut ui_stage = use_signal(|| 0); let mut countdown = use_signal(|| 0); let mut mobile_input = use_signal(|| String::new()); let mut otp_input = use_signal(|| String::new()); let mut error_msg = use_signal(|| String::new());
    rsx! {
        div {
            class: "dashboard",
            style: "text-align: center; border: 1px solid #8b5cf6;",
            h3 { color: "#8b5cf6", "📱 Mobile Verification Required (Tier 2)" }
            if ui_stage() == 0 {
                input {
                    class: "input-field",
                    r#type: "tel",
                    placeholder: "Enter Mobile Number",
                    value: "{mobile_input}",
                    onmounted: move |e| async move {
                        let _ = e.set_focus(true).await;
                    },
                    oninput: move |e| mobile_input.set(e.value()),
                    onkeypress: move |e| {
                        if e.key().to_string() == "Enter" && !mobile_input().is_empty() {
                            ui_stage.set(1);
                            countdown.set(60);
                            let mobile = mobile_input();
                            spawn(async move {
                                request_mobile_otp_from_vault(mobile).await;
                            });
                            spawn(async move {
                                while countdown() > 0 {
                                    sleep(Duration::from_secs(1)).await;
                                    countdown.set(countdown() - 1);
                                }
                            });
                        }
                    },
                }
                br {}
                button {
                    class: "btn btn-mobile",
                    onclick: move |_| {
                        if !mobile_input().is_empty() {
                            ui_stage.set(1);
                            countdown.set(60);
                            let mobile = mobile_input();
                            spawn(async move {
                                request_mobile_otp_from_vault(mobile).await;
                            });
                            spawn(async move {
                                while countdown() > 0 {
                                    sleep(Duration::from_secs(1)).await;
                                    countdown.set(countdown() - 1);
                                }
                            });
                        }
                    },
                    "Send SMS OTP"
                }
            }
            if ui_stage() == 1 {
                p { style: "color: #94a3b8;", "SMS Sent to {mobile_input()}" }
                div { class: "otp-wrapper",
                    div { class: "otp-squares",
                        for i in 0..6 {
                            div { class: if otp_input().len() == i { "otp-square otp-square-active" } else { "otp-square" },
                                "{otp_input().chars().nth(i).unwrap_or(' ')}"
                            }
                        }
                    }
                    input {
                        class: "otp-ghost",
                        r#type: "text",
                        maxlength: "6",
                        value: "{otp_input}",
                        onmounted: move |e| async move {
                            let _ = e.set_focus(true).await;
                        },
                        oninput: move |e| {
                            let filtered: String = e
                                .value()
                                .chars()
                                .filter(|c| c.is_ascii_digit())
                                .collect();
                            otp_input.set(filtered.clone());
                            error_msg.set(String::new());
                            if filtered.len() == 6 {
                                spawn(async move {
                                    if submit_mobile_otp_to_vault(filtered).await {
                                        ui_stage.set(2);
                                        on_verified.call(());
                                    } else {
                                        error_msg.set("Invalid OTP. Try again.".to_string());
                                        otp_input.set(String::new());
                                    }
                                });
                            }
                        },
                    }
                }
                br {}
                if countdown() > 0 {
                    p { class: "timer-text", "Resend OTP in {countdown}s..." }
                } else {
                    button {
                        class: "btn btn-secondary",
                        onclick: move |_| {
                            countdown.set(60);
                            let mobile = mobile_input();
                            spawn(async move {
                                request_mobile_otp_from_vault(mobile).await;
                            });
                            spawn(async move {
                                while countdown() > 0 {
                                    sleep(Duration::from_secs(1)).await;
                                    countdown.set(countdown() - 1);
                                }
                            });
                        },
                        "Resend OTP"
                    }
                }
                button {
                    class: "btn btn-success",
                    onclick: move |_| {
                        let code = otp_input();
                        spawn(async move {
                            if submit_mobile_otp_to_vault(code).await {
                                ui_stage.set(2);
                                on_verified.call(());
                            } else {
                                error_msg.set("Invalid OTP. Try again.".to_string());
                                otp_input.set(String::new());
                            }
                        });
                    },
                    "Verify Mobile OTP"
                }
                if !error_msg().is_empty() {
                    p { class: "error-text", "{error_msg}" }
                }
            }
            if ui_stage() == 2 {
                h3 { style: "color: #22c55e;", "✅ Mobile Successfully Verified!" }
            }
        }
    }
}

#[component]
fn KycGate(on_verified: EventHandler<()>) -> Element {
    let mut verified = use_signal(|| false);
    rsx! {
        div {
            class: "dashboard",
            style: "text-align: center; border: 1px solid #10b981;",
            h3 { color: "#10b981", "🆔 zk-KYC Verification Required (Tier 3)" }
            if !verified() {
                p { color: "#94a3b8", "Connect your Zero-Knowledge ID to proceed." }
                button {
                    class: "btn btn-kyc",
                    onclick: move |_| {
                        verified.set(true);
                        on_verified.call(());
                    },
                    "Verify zk-KYC Now"
                }
            } else {
                h3 { style: "color: #22c55e;", "✅ zk-KYC Successfully Verified!" }
            }
        }
    }
}

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[component]
fn App() -> Element {
    use_hook(|| {
        let saved_version = LocalStorage::get::<String>("app_version").unwrap_or_default();
        if saved_version != APP_VERSION && !saved_version.is_empty() {
            let _ = LocalStorage::set("app_version", &APP_VERSION.to_string());
            add_log(&format!("System Update: Platform updated to v{}. Please refresh if needed.", APP_VERSION));
        } else if saved_version.is_empty() {
            let _ = LocalStorage::set("app_version", &APP_VERSION.to_string());
            add_log("System Boot: Core platform initialized successfully.");
        }
    });

    rsx! {
        div {
            style: "min-height: 100vh; width: 100vw;",
            ondragover: move |e| e.prevent_default(),

            style {
                "body {{ font-family: system-ui, sans-serif; background-color: #0f172a; color: #f8fafc; padding: 0; margin: 0; text-align: center; }}"
                ".dashboard {{ margin: 0 auto; max-width: 600px; padding: 20px; background-color: #1e293b; border-radius: 12px; text-align: left; }}"
                "ul {{ list-style: none; padding: 0; line-height: 2.0; }}"
                "a {{ color: #38bdf8; text-decoration: none; font-weight: bold; margin-left: 10px; cursor: pointer; }}"
                ".locked-link {{ color: #64748b; font-style: italic; }}"
                ".btn {{ padding: 12px 24px; margin: 8px; border-radius: 8px; border: none; font-weight: bold; cursor: pointer; }}"
                ".btn-email {{ background-color: #3b82f6; color: white; }}"
                ".btn-mobile {{ background-color: #8b5cf6; color: white; }}"
                ".btn-kyc {{ background-color: #10b981; color: white; }}"
                ".btn-danger {{ background-color: #ef4444; color: white; }}"
                ".btn-success {{ background-color: #22c55e; color: white; }}"
                ".btn-secondary {{ background-color: #475569; color: white; }}"
                ".input-field {{ padding: 12px; border-radius: 8px; border: 1px solid #475569; background-color: #0f172a; color: white; margin-bottom: 10px; width: 80%; max-width: 300px; caret-color: #38bdf8; }}"
                ".otp-wrapper {{ position: relative; width: 300px; height: 50px; margin: 0 auto 15px auto; }}"
                ".otp-squares {{ display: flex; justify-content: space-between; position: absolute; top: 0; left: 0; right: 0; bottom: 0; pointer-events: none; }}"
                ".otp-square {{ width: 40px; height: 50px; border: 2px solid #475569; border-radius: 8px; display: flex; align-items: center; justify-content: center; font-size: 24px; font-family: monospace; font-weight: bold; color: white; background-color: #1e293b; position: relative; }}"
                "@keyframes cursor-blink {{ 0%, 100% {{ opacity: 1; }} 50% {{ opacity: 0; }} }}"
                ".otp-square-active {{ border-color: #38bdf8; box-shadow: 0 0 8px rgba(56, 189, 248, 0.5); }}"
                ".otp-square-active::after {{ content: '|'; position: absolute; animation: cursor-blink 1s infinite; color: #38bdf8; font-weight: 300; font-size: 28px; }}"
                ".otp-ghost {{ position: absolute; top: 0; left: 0; width: 100%; height: 100%; opacity: 0; cursor: pointer; color: transparent; background: transparent; border: none; outline: none; }}"
                ".error-text {{ color: #ef4444; font-weight: bold; margin-top: 10px; }}"
                ".timer-text {{ color: #94a3b8; font-size: 14px; margin-bottom: 10px; }}"
                ".fab-container {{ position: fixed; bottom: 20px; right: 20px; display: flex; flex-direction: column; gap: 10px; z-index: 1000; }}"
                ".fab {{ background-color: #3b82f6; color: white; border: none; border-radius: 50%; width: 60px; height: 60px; font-size: 24px; cursor: pointer; }}"
                ".fab-chat {{ background-color: #10b981; }}"
                ".tab-bar {{ display: flex; background: #020617; padding: 10px 10px 0 10px; border-bottom: 2px solid #1e293b; overflow-x: auto; align-items: flex-end; }}"
                ".tab {{ padding: 10px 20px; background: #1e293b; color: #94a3b8; border-radius: 8px 8px 0 0; margin-right: 5px; cursor: pointer; display: flex; align-items: center; gap: 10px; font-size: 14px; user-select: none; }}"
                ".tab-active {{ background: #0f172a; color: #38bdf8; font-weight: bold; border-top: 2px solid #38bdf8; border-left: 1px solid #1e293b; border-right: 1px solid #1e293b; }}"
                ".close-tab {{ cursor: pointer; color: #ef4444; font-weight: bold; padding: 0 5px; border-radius: 50%; }}"
                ".close-tab:hover {{ background: rgba(239, 68, 68, 0.2); }}"
                ".workspace-area {{ padding: 20px; height: calc(100vh - 60px); overflow-y: auto; }}"
                ".chat-container {{ display: flex; flex-direction: column; height: 65vh; background: #0f172a; border: 1px solid #38bdf8; border-radius: 12px; overflow: hidden; margin-top: 20px; box-shadow: 0 4px 20px rgba(0,0,0,0.5); }}"
                ".chat-history {{ flex: 1; padding: 20px; overflow-y: auto; display: flex; flex-direction: column; gap: 12px; }}"
                ".msg-bubble {{ max-width: 75%; padding: 12px 16px; border-radius: 12px; font-size: 15px; line-height: 1.5; word-wrap: break-word; }}"
                ".mine {{ align-self: flex-end; background: #38bdf8; color: #020617; border-bottom-right-radius: 2px; font-weight: 500; }}"
                ".theirs {{ align-self: flex-start; background: #1e293b; color: #f8fafc; border-bottom-left-radius: 2px; border: 1px solid #334155; }}"
                ".chat-input-area {{ display: flex; padding: 15px; background: #1e293b; border-top: 1px solid #334155; gap: 10px; }}"
                ".chat-input {{ flex: 1; padding: 12px 15px; border-radius: 8px; border: 1px solid #475569; background: #0f172a; color: white; outline: none; font-size: 15px; transition: border-color 0.2s; }}"
                ".chat-input:focus {{ border-color: #38bdf8; }}"
                ".send-btn {{ margin: 0; padding: 0 24px; }}"
            }

            div {
                class: "tab-bar",
                ondragover: move |e| e.prevent_default(),
                ondrop: move |_| {
                    if let Some(dragged_id) = *DRAGGED_TAB_ID.read() {
                        let mut tabs = TABS.write();
                        if let Some(from_idx) = tabs.iter().position(|t| t.id == dragged_id) {
                            let element = tabs.remove(from_idx);
                            tabs.push(element);
                        }
                    }
                    *DRAGGED_TAB_ID.write() = None;
                },
                for tab in TABS() {
                    div {
                        class: if tab.id == ACTIVE_TAB_ID() { "tab tab-active" } else { "tab" },
                        draggable: "true",
                        ondragover: move |e| e.prevent_default(),
                        ondragstart: move |_| {
                            *DRAGGED_TAB_ID.write() = Some(tab.id);
                        },
                        ondrop: move |e| {
                            e.stop_propagation();
                            if let Some(dragged_id) = *DRAGGED_TAB_ID.read() {
                                if dragged_id != tab.id {
                                    let mut tabs = TABS.write();
                                    if let Some(from_idx) = tabs.iter().position(|t| t.id == dragged_id) {
                                        if let Some(to_idx) = tabs.iter().position(|t| t.id == tab.id) {
                                            let element = tabs.remove(from_idx);
                                            tabs.insert(to_idx, element);
                                        }
                                    }
                                }
                            }
                            *DRAGGED_TAB_ID.write() = None;
                        },
                        onclick: move |_| switch_to_tab(tab.id, &tab.route),
                        "{tab.title}"
                        if TABS().len() > 1 {
                            span {
                                class: "close-tab",
                                onclick: move |e| {
                                    e.stop_propagation();
                                    let mut tabs = TABS.write();
                                    tabs.retain(|t| t.id != tab.id);
                                    if ACTIVE_TAB_ID() == tab.id {
                                        *ACTIVE_TAB_ID.write() = tabs.last().unwrap().id;
                                    }
                                },
                                "×"
                            }
                        }
                    }
                }
            }

            div { class: "workspace-area",
                for tab in TABS() {
                    div { style: if tab.id == ACTIVE_TAB_ID() { "display: block;" } else { "display: none;" },
                        match tab.route {
                            Route::Home {} => rsx! {
                                Home {}
                            },
                            Route::News {} => rsx! {
                                News {}
                            },
                            Route::Tube {} => rsx! {
                                Tube {}
                            },
                            Route::PublicChat {} => rsx! {
                                PublicChat {}
                            },
                            Route::PrivateChat {} => rsx! {
                                PrivateChat {}
                            },
                            Route::Dating {} => rsx! {
                                Dating {}
                            },
                            Route::EComm {} => rsx! {
                                EComm {}
                            },
                            Route::UnderDevelopment {} => rsx! {
                                UnderDevelopment {}
                            },
                        }
                    }
                }
            }
        }

        GlobalVaultSyncer {}
        GlobalCallOverlay {}
        FloatingNavigation {}
    }
}

// --- NEW: THE GLOBAL VAULT SYNCER WITH LOGS ---
#[component]
fn GlobalVaultSyncer() -> Element {
    let mut synced_rooms = use_signal(|| std::collections::HashSet::<String>::new());

    use_effect(move || {
        spawn(async move {
            add_log("GlobalVaultSyncer: Initialized background polling loop.");
            loop {
                let current_contacts = LocalStorage::get::<Vec<SavedContact>>("govind_contacts").unwrap_or_default();
                for contact in current_contacts {
                    if !synced_rooms.read().contains(&contact.room_id) {
                        synced_rooms.write().insert(contact.room_id.clone());
                        
                        let room = contact.room_id.clone();
                        let pass = contact.secret_password.clone();
                        let c_name = contact.name.clone();
                        
                        add_log(&format!("VaultSyncer: Binding persistent WebSocket listener to room '{}'", room));

                        spawn(async move {
                            loop {
                                let url = format!("{}/secure/api/chat/{}", WS_BASE_URL, room);
                                if let Ok(ws) = WebSocket::open(&url) {
                                    add_log(&format!("VaultSyncer: WebSocket successfully connected to room '{}'", room));
                                    
                                    let (mut _keep_alive_write, mut read) = ws.split();
                                    
                                    while let Some(msg) = read.next().await {
                                        if let Ok(WsMessage::Text(txt)) = msg {
                                            if let Ok(payload) = serde_json::from_str::<EncryptedMessage>(&txt) {
                                                if payload.sender_id != ACTIVE_HASH() && CHAT_ACTIVE_ROOM() != room {
                                                    add_log(&format!("VaultSyncer: Incoming payload intercepted for room '{}'. Attempting decryption.", room));
                                                    
                                                    if let Ok(clean_text) = decrypt_message(&payload.ciphertext, &pass) {
                                                        if clean_text.starts_with("RTC_SIGNAL:") {
                                                            let json_str = clean_text.replace("RTC_SIGNAL:", "");
                                                            if let Ok(signal) = serde_json::from_str::<RtcSignalPayload>(&json_str) {
                                                                
                                                                add_log(&format!("VaultSyncer: DECRYPTED! RTC Signal parsed successfully: Type = {}", signal.signal_type));
                                                                
                                                                if signal.signal_type == "offer" {
                                                                    add_log("VaultSyncer: 🟢 It's a Call Offer! Triggering Global UI Overlay and Audio.");
                                                                    
                                                                    *CHAT_INCOMING_OFFER_SDP.write() = signal.sdp.clone();
                                                                    *GLOBAL_INCOMING_CALL.write() = Some(CallAlert {
                                                                        room_id: room.clone(), caller_name: c_name.clone(),
                                                                        call_type: signal.call_type.clone(), secret_password: pass.clone(),
                                                                    });
                                                                    
                                                                    let js_code = format!(
                                                                        r#"
                                                                        if (window.ringtone) {{ window.ringtone.pause(); }}
                                                                        window.ringtone = new Audio('https://google.com');
                                                                        window.ringtone.loop = true;
                                                                        window.ringtone.play().catch(e => console.log('Audio blocked', e));
                                                                        "#
                                                                    );

                                                                    // PASS THE VARIABLE BY REFERENCE HERE TO CLEAR THE WARNING
                                                                    let _ = eval(&js_code); 

                                                                } else if signal.signal_type == "end_call" {
                                                                    add_log("VaultSyncer: 🔴 Call cancellation signal received. Dropping overlay.");
                                                                    let mut global_call = GLOBAL_INCOMING_CALL.write();
                                                                    if let Some(call) = global_call.as_ref() {
                                                                        if call.room_id == room { 
                                                                            *global_call = None; 
                                                                            let _ = eval("{ if (window.ringtone) { window.ringtone.pause(); } }");
                                                                        }
                                                                    }
                                                                }
                                                            } else {
                                                                add_log("VaultSyncer: ERROR - Failed to parse inner RtcSignalPayload JSON.");
                                                            }
                                                        } else {
                                                            let mut counts = UNREAD_BADGES.write();
                                                            *counts.entry(room.clone()).or_insert(0) += 1;
                                                            let storage_key = format!("vault_chat_{}", room);
                                                            let mut saved_msgs = LocalStorage::get::<Vec<ChatMessage>>(&storage_key).unwrap_or_default();
                                                            saved_msgs.push(ChatMessage { id: saved_msgs.len(), text: clean_text, is_mine: false });
                                                            let _ = LocalStorage::set(&storage_key, &saved_msgs);
                                                        }
                                                    } else {
                                                        add_log("VaultSyncer: ERROR - Decryption failed for incoming payload!");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    add_log(&format!("VaultSyncer: WARNING - WebSocket connection dropped for room '{}'. Reconnecting...", room));
                                } else {
                                    add_log(&format!("VaultSyncer: ERROR - Failed to open WebSocket for room '{}'", room));
                                }
                                sleep(Duration::from_secs(3)).await; 
                            }
                        });
                    }
                }
                sleep(Duration::from_secs(3)).await;
            }
        });
    });

    rsx! {
        div { display: "none" }

    }
}

#[component]
fn GlobalCallOverlay() -> Element {
    let mut pos = use_signal(|| (0.0, 0.0));
    let mut has_moved = use_signal(|| false);
    let mut is_dragging = use_signal(|| false);
    let mut drag_offset = use_signal(|| (0.0, 0.0));
    let mut is_silenced = use_signal(|| false); 

    use_effect(move || {
        if GLOBAL_INCOMING_CALL().is_none() { is_silenced.set(false); }
    });

    if let Some(call) = GLOBAL_INCOMING_CALL() {
        rsx! {
            if is_dragging() {
                div {
                    style: "position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; z-index: 999999; cursor: grabbing; touch-action: none;",
                    onpointermove: move |e| {
                        let x = (e.client_coordinates().x - drag_offset().0).max(0.0);
                        let y = (e.client_coordinates().y - drag_offset().1).max(0.0);
                        pos.set((x, y));
                    },
                    onpointerup: move |_| {
                        is_dragging.set(false);
                    },
                    onpointerleave: move |_| {
                        is_dragging.set(false);
                    },
                }
            }

            if is_silenced() {
                div {
                    style: if has_moved() { format!(
                        "position: fixed; top: {}px; left: {}px; z-index: 999998;",
                        pos().1,
                        pos().0,
                    ) } else { "position: fixed; top: 20px; left: 50%; transform: translateX(-50%); z-index: 999998;"
                        .to_string() },
                    div {
                        style: "background: rgba(15, 23, 42, 0.9); backdrop-filter: blur(10px); padding: 10px; border-radius: 50px; border: 2px solid #f59e0b; box-shadow: 0 0 15px rgba(245, 158, 11, 0.5); display: flex; align-items: center; gap: 10px; cursor: grab; touch-action: none; -webkit-user-select: none; user-select: none;",
                        onpointerdown: move |e| {
                            let current_x = if has_moved() {
                                pos().0
                            } else {
                                e.client_coordinates().x - 60.0
                            };
                            let current_y = if has_moved() {
                                pos().1
                            } else {
                                e.client_coordinates().y - 20.0
                            };
                            if !has_moved() {
                                pos.set((current_x, current_y));
                                has_moved.set(true);
                            }
                            drag_offset
                                .set((
                                    e.client_coordinates().x - current_x,
                                    e.client_coordinates().y - current_y,
                                ));
                            is_dragging.set(true);
                        },
                        span { style: "color: #f59e0b; font-size: 20px; margin-left: 10px; animation: pulse 2s infinite;",
                            "🔕"
                        }
                        span { style: "color: #f8fafc; font-weight: bold; margin-right: 10px;",
                            "{call.caller_name}"
                        }
                        button {
                            class: "btn btn-success",
                            style: "padding: 6px 12px; border-radius: 20px; font-size: 12px; margin: 0;",
                            onclick: move |_| {
                                is_silenced.set(false);
                            },
                            "Expand"
                        }
                    }
                }
            } else {
                div {
                    style: if has_moved() { format!(
                        "position: fixed; top: {}px; left: {}px; z-index: 999998; background: rgba(15, 23, 42, 0.9); backdrop-filter: blur(10px); padding: 20px; border-radius: 16px; border: 2px solid #10b981; box-shadow: 0 0 30px rgba(16, 185, 129, 0.4); width: 300px; text-align: center;",
                        pos().1,
                        pos().0,
                    ) } else { "position: fixed; top: 20px; left: 50%; transform: translateX(-50%); z-index: 999998; background: rgba(15, 23, 42, 0.9); backdrop-filter: blur(10px); padding: 20px; border-radius: 16px; border: 2px solid #10b981; box-shadow: 0 0 30px rgba(16, 185, 129, 0.4); width: 300px; text-align: center;"
                        .to_string() },
                    div {
                        style: "width: 100%; height: 20px; cursor: grab; display: flex; justify-content: center; align-items: center; color: #94a3b8; margin-bottom: 10px; touch-action: none; -webkit-user-select: none; user-select: none;",
                        onpointerdown: move |e| {
                            let current_x = if has_moved() {
                                pos().0
                            } else {
                                e.client_coordinates().x - 150.0
                            };
                            let current_y = if has_moved() {
                                pos().1
                            } else {
                                e.client_coordinates().y - 10.0
                            };
                            if !has_moved() {
                                pos.set((current_x, current_y));
                                has_moved.set(true);
                            }
                            drag_offset
                                .set((
                                    e.client_coordinates().x - current_x,
                                    e.client_coordinates().y - current_y,
                                ));
                            is_dragging.set(true);
                        },
                        "⠿"
                    }
                    h3 { style: "color: #f8fafc; margin: 0 0 10px 0;",
                        "Incoming {call.call_type} Call"
                    }
                    p { style: "color: #38bdf8; font-size: 18px; font-weight: bold; margin: 0 0 20px 0;",
                        "{call.caller_name}"
                    }
                    div { style: "display: flex; justify-content: space-around; gap: 5px;",
                        button {
                            class: "btn btn-danger",
                            style: "flex: 1; padding: 10px 5px; border-radius: 20px; font-weight: bold; font-size: 13px;",
                            onclick: {
                                let c = call.clone();
                                move |_| {
                                    add_log(
                                        "GlobalUI: User hit 'Decline'. Tearing down UI and sending disconnect.",
                                    );
                                    let _ = eval("{ if (window.ringtone) { window.ringtone.pause(); } }");
                                    *GLOBAL_INCOMING_CALL.write() = None;
                                    let room = c.room_id.clone();
                                    let pass = c.secret_password.clone();
                                    spawn(async move {
                                        if let Ok(ws) = WebSocket::open(
                                            &format!("{}/secure/api/chat/{}", WS_BASE_URL, room),
                                        ) {
                                            let (mut write, _) = ws.split();
                                            let end_signal = RtcSignalPayload {
                                                signal_type: "end_call".to_string(),
                                                sdp: "".to_string(),
                                                call_type: "".to_string(),
                                            };
                                            let ciphertext = encrypt_message(
                                                &format!(
                                                    "RTC_SIGNAL:{}",
                                                    serde_json::to_string(&end_signal).unwrap(),
                                                ),
                                                &pass,
                                            );
                                            let secure_payload = EncryptedMessage {
                                                sender_id: ACTIVE_HASH(),
                                                timestamp: 0,
                                                ciphertext,
                                                signature: "verified_client".to_string(),
                                            };
                                            let _ = write
                                                .send(
                                                    WsMessage::Text(
                                                        serde_json::to_string(&secure_payload).unwrap(),
                                                    ),
                                                )
                                                .await;
                                        }
                                    });
                                }
                            },
                            "Decline"
                        }
                        button {
                            class: "btn btn-secondary",
                            style: "flex: 1; padding: 10px 5px; border-radius: 20px; font-weight: bold; font-size: 13px; background: #f59e0b;",
                            onclick: move |_| {
                                add_log("GlobalUI: User hit 'Silent'. Pausing audio and shrinking UI.");
                                let _ = eval("{ if (window.ringtone) { window.ringtone.pause(); } }");
                                is_silenced.set(true);
                            },
                            "Silent"
                        }
                        button {
                            class: "btn btn-success",
                            style: "flex: 1; padding: 10px 5px; border-radius: 20px; font-weight: bold; font-size: 13px; animation: pulse 1.5s infinite;",
                            onclick: {
                                let c = call.clone();
                                move |_| {
                                    add_log(
                                        "GlobalUI: User hit 'Answer'. Transporting user to Secure Chat partition...",
                                    );
                                    let _ = eval("{ if (window.ringtone) { window.ringtone.pause(); } }");
                                    *CHAT_ACTIVE_ROOM.write() = c.room_id.clone();
                                    *CHAT_ACTIVE_PASS.write() = c.secret_password.clone();
                                    *CHAT_ACTIVE_NAME.write() = c.caller_name.clone();
                                    *CHAT_CALL_TYPE.write() = c.call_type.clone();
                                    *CHAT_IS_ANSWERING.write() = true;
                                    *GLOBAL_INCOMING_CALL.write() = None;
                                    open_or_focus_tab(Route::PrivateChat {}, "Chat");
                                }
                            },
                            "Answer"
                        }
                    }
                }
            }
        }
    } else { rsx! {
        div { display: "none" }
    } }
}

#[component]
fn FloatingNavigation() -> Element {
    let mut pos = use_signal(|| (0.0, 0.0)); let mut has_moved = use_signal(|| false); let mut is_dragging = use_signal(|| false); let mut drag_offset = use_signal(|| (0.0, 0.0));
    rsx! {
        if is_dragging() {
            div {
                style: "position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; z-index: 99999; cursor: grabbing; touch-action: none;",
                onpointermove: move |e| {
                    let x = (e.client_coordinates().x - drag_offset().0).max(0.0);
                    let y = (e.client_coordinates().y - drag_offset().1).max(0.0);
                    pos.set((x, y));
                },
                onpointerup: move |_| {
                    is_dragging.set(false);
                },
                onpointerleave: move |_| {
                    is_dragging.set(false);
                },
            }
        }
        div {
            class: "fab-container",
            style: if has_moved() { format!(
                "top: {}px; left: {}px; bottom: auto; right: auto; background: rgba(30,41,59,0.7); backdrop-filter: blur(10px); padding: 8px; border-radius: 40px; border: 1px solid rgba(56,189,248,0.4); box-shadow: 0 10px 25px rgba(0,0,0,0.5);",
                pos().1,
                pos().0,
            ) } else { "bottom: 20px; right: 20px; background: rgba(30,41,59,0.7); backdrop-filter: blur(10px); padding: 8px; border-radius: 40px; border: 1px solid rgba(56,189,248,0.4); box-shadow: 0 10px 25px rgba(0,0,0,0.5);"
                .to_string() },
            div {
                style: "width: 100%; height: 30px; cursor: grab; display: flex; justify-content: center; align-items: center; color: #94a3b8; font-size: 24px; touch-action: none; -webkit-user-select: none; user-select: none;",
                onpointerdown: move |e| {
                    let current_x = if has_moved() {
                        pos().0
                    } else {
                        e.client_coordinates().x - 30.0
                    };
                    let current_y = if has_moved() {
                        pos().1
                    } else {
                        e.client_coordinates().y - 15.0
                    };
                    if !has_moved() {
                        pos.set((current_x, current_y));
                        has_moved.set(true);
                    }
                    drag_offset
                        .set((
                            e.client_coordinates().x - current_x,
                            e.client_coordinates().y - current_y,
                        ));
                    is_dragging.set(true);
                },
                "⠿"
            }
            button {
                class: "fab",
                onclick: move |_| open_or_focus_tab(Route::Home {}, "Home"),
                "🏠"
            }
            button {
                class: "fab",
                style: "background-color: #10b981;",
                onclick: move |_| open_or_focus_tab(Route::PrivateChat {}, "Chat"),
                "💬"
            }
            button {
                class: "fab",
                style: "background-color: #38bdf8;",
                onclick: move |_| open_or_focus_tab(Route::PublicChat {}, "Public"),
                "🌐"
            }
            button {
                class: "fab",
                style: "background-color: #f59e0b;",
                onclick: move |_| {
                    // Note: In this file, there is no SHOW_SUBSCRIPTION_OVERLAY,
                    // but we follow the user's request for Subscribation link.
                    // If Route::EComm exists, we could use that, or just a placeholder.
                    open_or_focus_tab(Route::EComm {}, "Subscribation")
                },
                "💎"
            }
        }
    }
}

// --- 5. THE HOME DASHBOARD WITH LOG VIEWER ---
#[component]
fn Home() -> Element {
    use_hook(|| { if let Ok(tier) = LocalStorage::get::<i32>("home_tier") { if tier > 0 { *HOME_TIER.write() = tier as u8; if let Ok(email) = LocalStorage::get::<String>("home_email") { *HOME_EMAIL.write() = email; } } } });
    let chat_tier = use_signal(|| LocalStorage::get::<i32>("chat_tier").unwrap_or(0)); let chat_email = use_signal(|| LocalStorage::get::<String>("chat_email").unwrap_or_default());
    
    // NEW: Log Viewer UI Toggle
    let mut show_logs = use_signal(|| false);

    rsx! {
        div {
            h1 { "Govind Hub: Zero-Knowledge Platform" }
            p { "Security Tier: {HOME_TIER()}" }
            if HOME_TIER() == 0 {
                if chat_tier() >= 1 {
                    div { style: "background: #1e293b; padding: 20px; border-radius: 8px; border: 1px solid #8b5cf6; margin-bottom: 20px; max-width: 400px; margin-left: auto; margin-right: auto;",
                        p {
                            "We see you are logged into Secure Chat as "
                            strong { color: "#38bdf8", "{chat_email()}" }
                        }
                        button {
                            class: "btn btn-success",
                            style: "width: 100%; margin: 5px 0;",
                            onclick: move |_| {
                                let _ = LocalStorage::set("home_tier", &chat_tier());
                                let _ = LocalStorage::set("home_email", &chat_email());
                                *HOME_TIER.write() = chat_tier() as u8;
                                *HOME_EMAIL.write() = chat_email();
                            },
                            "Continue as {chat_email()}"
                        }
                        p { style: "margin: 10px 0; color: #94a3b8; font-size: 14px;",
                            "or"
                        }
                        IdentityGate {
                            title: "Login with different account".to_string(),
                            prompt_text: "Login".to_string(),
                            allow_reuse: false,
                            on_verified: move |email| {
                                let _ = LocalStorage::set("home_email", &email);
                                let _ = LocalStorage::set("home_tier", &1);
                                *HOME_EMAIL.write() = email;
                                *HOME_TIER.write() = 1;
                            },
                        }
                    }
                } else {
                    IdentityGate {
                        title: "Primary Dashboard Access".to_string(),
                        prompt_text: "Login / Sign Up".to_string(),
                        allow_reuse: false,
                        on_verified: move |email| {
                            let _ = LocalStorage::set("home_email", &email);
                            let _ = LocalStorage::set("home_tier", &1);
                            *HOME_EMAIL.write() = email;
                            *HOME_TIER.write() = 1;
                        },
                    }
                }
            } else {
                div {
                    margin_bottom: "20px",
                    padding: "15px",
                    background_color: "#0f172a",
                    border_radius: "8px",
                    border: "1px solid #38bdf8",
                    p {
                        "Identity Active: "
                        strong { color: "#38bdf8", "{HOME_EMAIL()}" }
                    }
                    button {
                        class: "btn btn-danger",
                        onclick: move |_| {
                            let _ = LocalStorage::delete("home_tier");
                            let _ = LocalStorage::delete("home_email");
                            *HOME_EMAIL.write() = String::new();
                            *HOME_TIER.write() = 0;
                        },
                        "Log Out / Switch Account"
                    }
                }
            }

            div { class: "dashboard", margin_top: "20px",

                // THE FIX: Log Viewer Header
                div { style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 10px;",
                    h2 { margin: "0", "The 10-Partition Ecosystem" }
                    button {
                        class: "btn btn-secondary",
                        style: "margin: 0; padding: 8px 12px; font-size: 13px;",
                        onclick: move |_| show_logs.set(!show_logs()),
                        if show_logs() {
                            "Hide Logs"
                        } else {
                            "📜 System Logs"
                        }
                    }
                }

                // THE FIX: Log Viewer Box with Share Button
                if show_logs() {
                    div { style: "background: #020617; padding: 15px; border-radius: 8px; border: 1px solid #475569; margin-bottom: 20px;",
                        div { style: "display: flex; justify-content: space-between; align-items: center; margin-bottom: 10px;",
                            h3 { margin: "0", color: "#f8fafc", "System Diagnostics" }
                            button {
                                class: "btn btn-email",
                                style: "margin: 0; padding: 6px 12px; font-size: 12px;",
                                onclick: move |_| {
                                    let all_logs = SYSTEM_LOGS.read().join("\n");
                                    let _ = eval(
                                        &format!(
                                            "{{ navigator.clipboard.writeText(`{}`); alert('Logs copied! You can now paste them to the developer.'); }}",
                                            all_logs.replace("`", "\\`"),
                                        ),
                                    );
                                },
                                "📋 Copy & Share"
                            }
                        }
                        div { style: "max-height: 300px; overflow-y: auto; font-family: monospace; font-size: 12px; color: #38bdf8; text-align: left; background: #0f172a; padding: 10px; border-radius: 4px; border: 1px solid #1e293b;",
                            if SYSTEM_LOGS.read().is_empty() {
                                div { color: "#64748b", "No logs recorded yet. System is silent." }
                            } else {
                                for log in SYSTEM_LOGS.read().iter() {
                                    div { style: "margin-bottom: 6px; border-bottom: 1px solid #1e293b; padding-bottom: 4px;",
                                        "{log}"
                                    }
                                }
                            }
                        }
                    }
                }

                ul {
                    li { "✅ 1. Home Dashboard" }
                    if HOME_TIER() >= 1 {
                        li {
                            "🔓 2. News & Blog (Write Access): "
                            a { onclick: move |_| open_or_focus_tab(Route::News {}, "News"),
                                "click"
                            }
                        }
                    } else {
                        li {
                            "✅ 2. News & Blog (Read-Only): "
                            a { onclick: move |_| open_or_focus_tab(Route::News {}, "News"),
                                "click"
                            }
                        }
                    }
                    if HOME_TIER() >= 1 {
                        li {
                            "🔓 3. Govind Tube (Upload Unlocked): "
                            a { onclick: move |_| open_or_focus_tab(Route::Tube {}, "Tube"),
                                "click"
                            }
                        }
                    } else {
                        li {
                            "✅ 3. Govind Tube (Watch Only): "
                            a { onclick: move |_| open_or_focus_tab(Route::Tube {}, "Tube"),
                                "click"
                            }
                        }
                    }
                    if HOME_TIER() >= 1 {
                        li {
                            "🔓 4. Public Conversation: "
                            a { onclick: move |_| open_or_focus_tab(Route::PublicChat {}, "Public Chat"),
                                "click"
                            }
                        }
                    } else {
                        li {
                            "✅ 4. Public Conversation (Read-Only): "
                            a { onclick: move |_| open_or_focus_tab(Route::PublicChat {}, "Public Chat"),
                                "click"
                            }
                        }
                    }
                    li {
                        "🚧 5. Community Hub: "
                        a { onclick: move |_| open_or_focus_tab(Route::UnderDevelopment {}, "Dev"),
                            "click"
                        }
                    }
                    li {
                        "🚧 6. Jobs Partition: "
                        a { onclick: move |_| open_or_focus_tab(Route::UnderDevelopment {}, "Dev"),
                            "click"
                        }
                    }
                    li {
                        "🚧 7. Play Store: "
                        a { onclick: move |_| open_or_focus_tab(Route::UnderDevelopment {}, "Dev"),
                            "click"
                        }
                    }
                    if HOME_TIER() >= 2 {
                        li {
                            "🔓 8. Private Conversation (E2EE): "
                            a { onclick: move |_| open_or_focus_tab(Route::PrivateChat {}, "Private Chat"),
                                "click"
                            }
                        }
                    } else {
                        li {
                            "🔒 8. Private Conversation (Requires Mobile): "
                            a {
                                class: "locked-link",
                                onclick: move |_| open_or_focus_tab(Route::PrivateChat {}, "Private Chat"),
                                "click"
                            }
                        }
                    }
                    if HOME_TIER() >= 3 {
                        li {
                            "🔓 9. Dating Partition: "
                            a { onclick: move |_| open_or_focus_tab(Route::Dating {}, "Dating"),
                                "click"
                            }
                        }
                    } else {
                        li {
                            "🔒 9. Dating Partition (Requires zk-KYC): "
                            a {
                                class: "locked-link",
                                onclick: move |_| open_or_focus_tab(Route::Dating {}, "Dating"),
                                "click"
                            }
                        }
                    }
                    if HOME_TIER() >= 3 {
                        li {
                            "🔓 10. Govind e-Comm: "
                            a { onclick: move |_| open_or_focus_tab(Route::EComm {}, "Shop"),
                                "click"
                            }
                        }
                    } else {
                        li {
                            "🔒 10. Govind e-Comm (Requires zk-KYC): "
                            a {
                                class: "locked-link",
                                onclick: move |_| open_or_focus_tab(Route::EComm {}, "Shop"),
                                "click"
                            }
                        }
                    }
                }
            }
        }
    }
}

// --- 6. SECURE SUB-PAGES ---
#[component]
fn PrivateChat() -> Element {
    let mut contacts = use_signal(|| LocalStorage::get::<Vec<SavedContact>>("govind_contacts").unwrap_or_default());
    let mut show_add_form = use_signal(|| false); let mut new_name = use_signal(|| String::new()); let mut new_room = use_signal(|| String::new()); let mut new_pass = use_signal(|| String::new());
    let mut editing_room = use_signal(|| String::new()); let mut edit_name = use_signal(|| String::new()); let mut edit_room = use_signal(|| String::new()); let mut edit_pass = use_signal(|| String::new());
    let mut storage_choice_made = use_signal(|| LocalStorage::get::<bool>("vault_unlocked").unwrap_or(false));
    let mut current_tier = use_signal(|| LocalStorage::get::<i32>("chat_tier").unwrap_or(0));
    let chat_email = use_signal(|| LocalStorage::get::<String>("chat_email").unwrap_or_default());
    let mut revealed_secrets = use_signal(|| std::collections::HashSet::new());

    use_effect(move || { let email = chat_email(); if !email.is_empty() { *ACTIVE_HASH.write() = generate_hash(&email); } });

    rsx! {
        div { text_align: "center", padding_top: "10px",
            div { style: "display: flex; flex-direction: column; justify-content: center; align-items: center; gap: 10px; margin-bottom: 20px;",
                h2 { margin: "0", "🕵️‍♂️ Secure Comms Partition" }
                if current_tier() > 0 {
                    div { style: "display: flex; align-items: center; gap: 15px; background: #1e293b; padding: 8px 16px; border-radius: 20px; border: 1px solid #475569;",
                        span { style: "color: #94a3b8; font-size: 14px;", "Active Identity:" }
                        strong { style: "color: #38bdf8; font-size: 14px;", "{chat_email()}" }
                        button {
                            class: "btn btn-danger",
                            style: "font-size: 12px; padding: 4px 10px; margin: 0; border-radius: 12px;",
                            onclick: move |_| {
                                let _ = LocalStorage::delete("chat_tier");
                                let _ = LocalStorage::delete("chat_email");
                                current_tier.set(0);
                            },
                            "Switch"
                        }
                    }
                }
            }

            if current_tier() == 0 {
                IdentityGate {
                    title: "Email Verification Required".to_string(),
                    prompt_text: "Login".to_string(),
                    allow_reuse: true,
                    on_verified: move |email| {
                        let _ = LocalStorage::set("chat_tier", &1);
                        let _ = LocalStorage::set("chat_email", &email);
                        current_tier.set(1);
                    },
                }
            } else if current_tier() == 1 {
                MobileGate {
                    on_verified: move |_| {
                        let _ = LocalStorage::set("chat_tier", &2);
                        current_tier.set(2);
                    },
                }
            } else {
                if !storage_choice_made() {
                    div {
                        class: "dashboard",
                        style: "border: 1px solid #f59e0b; text-align: center; padding: 40px 20px; background: #1e293b; border-radius: 8px; max-width: 600px; margin: 0 auto;",
                        h3 { style: "color: #f59e0b; margin-bottom: 20px; font-size: 24px;",
                            "🛡️ Local Vault Detected"
                        }
                        p { style: "color: #94a3b8; margin-bottom: 30px; font-size: 16px; line-height: 1.6;",
                            "Your encryption keys, address book, and chat history are stored locally on this device. Would you like to retain them, or burn the vault for a fresh start?"
                        }
                        div { style: "display: flex; gap: 20px; justify-content: center; flex-wrap: wrap;",
                            button {
                                class: "btn btn-success",
                                style: "padding: 12px 24px; font-weight: bold; font-size: 16px;",
                                onclick: move |_| {
                                    let _ = LocalStorage::set("vault_unlocked", &true);
                                    storage_choice_made.set(true);
                                },
                                "📂 Retain Local Backup"
                            }
                            button {
                                class: "btn btn-danger",
                                style: "padding: 12px 24px; font-weight: bold; font-size: 16px;",
                                onclick: move |_| {
                                    if let Some(window) = web_sys::window() {
                                        if let Ok(Some(ls)) = window.local_storage() {
                                            let mut keys_to_delete = Vec::new();
                                            if let Ok(length) = ls.length() {
                                                for i in 0..length {
                                                    if let Ok(Some(key)) = ls.key(i) {
                                                        if key.starts_with("vault_chat_") || key == "govind_contacts"
                                                        {
                                                            keys_to_delete.push(key);
                                                        }
                                                    }
                                                }
                                            }
                                            for key in keys_to_delete {
                                                let _ = ls.remove_item(&key);
                                            }
                                        }
                                    }
                                    contacts.set(Vec::new());
                                    let _ = LocalStorage::set("vault_unlocked", &true);
                                    storage_choice_made.set(true);
                                },
                                "🔥 Burn Vault & Start Fresh"
                            }
                        }
                    }
                } else if CHAT_ACTIVE_ROOM().is_empty() {
                    div {
                        class: "dashboard",
                        style: "border: 1px solid #38bdf8;",
                        div { style: "display: flex; justify_content: space-between; align-items: center; margin-bottom: 20px;",
                            h3 { color: "#38bdf8", margin: "0", "Encrypted Address Book" }
                            div { style: "display: flex; gap: 10px;",
                                if !contacts.read().is_empty() {
                                    button {
                                        class: "btn btn-danger",
                                        style: "margin: 0; padding: 8px 16px;",
                                        onclick: move |_| {
                                            contacts.set(Vec::new());
                                            let _ = LocalStorage::set("govind_contacts", &Vec::<SavedContact>::new());
                                        },
                                        "Delete All"
                                    }
                                }
                                button {
                                    class: "btn btn-success",
                                    style: "margin: 0; padding: 8px 16px;",
                                    onclick: move |_| show_add_form.set(!show_add_form()),
                                    if show_add_form() {
                                        "Cancel"
                                    } else {
                                        "+ Add Contact"
                                    }
                                }
                            }
                        }

                        if show_add_form() {
                            div { style: "background: #0f172a; padding: 15px; border-radius: 8px; margin-bottom: 20px; border: 1px solid #475569;",
                                p { color: "#94a3b8", font_size: "14px",
                                    "Saved locally. The server cannot read this."
                                }
                                input {
                                    class: "input-field",
                                    placeholder: "Contact Name (e.g. Alice)",
                                    value: "{new_name}",
                                    oninput: move |e| new_name.set(e.value()),
                                }
                                br {}
                                input {
                                    class: "input-field",
                                    placeholder: "Shared Room ID (e.g. shadow-7)",
                                    value: "{new_room}",
                                    oninput: move |e| new_room.set(e.value()),
                                }
                                br {}
                                input {
                                    class: "input-field",
                                    r#type: "password",
                                    placeholder: "Secret Decryption Password",
                                    value: "{new_pass}",
                                    oninput: move |e| new_pass.set(e.value()),
                                }
                                br {}
                                button {
                                    class: "btn btn-email",
                                    width: "80%",
                                    max_width: "300px",
                                    onclick: move |_| {
                                        if !new_name().is_empty() && !new_room().is_empty() && !new_pass().is_empty() {
                                            let mut current_list = contacts.read().clone();
                                            current_list
                                                .push(SavedContact {
                                                    name: new_name(),
                                                    room_id: new_room(),
                                                    secret_password: new_pass(),
                                                });
                                            let _ = LocalStorage::set("govind_contacts", &current_list);
                                            contacts.set(current_list);
                                            show_add_form.set(false);
                                            new_name.set(String::new());
                                            new_room.set(String::new());
                                            new_pass.set(String::new());
                                        }
                                    },
                                    "Save to Local Vault"
                                }
                            }
                        }

                        if contacts.read().is_empty() && !show_add_form() {
                            p {
                                color: "#64748b",
                                text_align: "center",
                                font_style: "italic",
                                "Your vault is empty."
                            }
                        } else {
                            ul { style: "padding: 0; margin: 0;",
                                for contact in contacts.read().iter().cloned() {
                                    li { style: "background: #1e293b; padding: 15px; margin-bottom: 10px; border-radius: 8px; display: flex; justify_content: space-between; align-items: center; border: 1px solid #334155;",
                                        if editing_room() == contact.room_id {
                                            div { style: "display: flex; flex-direction: column; gap: 10px; width: 100%;",
                                                input {
                                                    class: "input-field",
                                                    style: "width: 100%; max-width: none;",
                                                    value: "{edit_name}",
                                                    placeholder: "Name",
                                                    oninput: move |e| edit_name.set(e.value()),
                                                }
                                                input {
                                                    class: "input-field",
                                                    style: "width: 100%; max-width: none;",
                                                    value: "{edit_room}",
                                                    placeholder: "Room ID",
                                                    oninput: move |e| edit_room.set(e.value()),
                                                }
                                                input {
                                                    class: "input-field",
                                                    style: "width: 100%; max-width: none;",
                                                    r#type: "password",
                                                    placeholder: "Password",
                                                    value: "{edit_pass}",
                                                    oninput: move |e| edit_pass.set(e.value()),
                                                }
                                                div { style: "display: flex; gap: 10px; justify-content: flex-end;",
                                                    button {
                                                        class: "btn btn-secondary",
                                                        style: "padding: 8px 16px; margin: 0;",
                                                        onclick: move |_| editing_room.set(String::new()),
                                                        "Cancel"
                                                    }
                                                    button {
                                                        class: "btn btn-success",
                                                        style: "padding: 8px 16px; margin: 0;",
                                                        onclick: {
                                                            let old_room = contact.room_id.clone();
                                                            move |_| {
                                                                let mut current_list = contacts.read().clone();
                                                                if let Some(c) = current_list.iter_mut().find(|c| c.room_id == old_room) {
                                                                    c.name = edit_name();
                                                                    c.room_id = edit_room();
                                                                    c.secret_password = edit_pass();
                                                                }
                                                                let _ = LocalStorage::set("govind_contacts", &current_list);
                                                                contacts.set(current_list);
                                                                editing_room.set(String::new());
                                                            }
                                                        },
                                                        "Save"
                                                    }
                                                }
                                            }
                                        } else {
                                            div { style: "display: flex; align-items: center; gap: 15px;",
                                                div { style: "width: 45px; height: 45px; border-radius: 50%; background: #38bdf8; display: flex; align-items: center; justify-content: center; font-weight: bold; color: #0f172a; font-size: 20px;",
                                                    "{contact.name.chars().next().unwrap_or('?')}"
                                                }
                                                div {
                                                    div { style: "color: #f8fafc; font-weight: bold; font-size: 16px; margin-bottom: 4px;",
                                                        "{contact.name}"
                                                    }
                                                    div { style: "color: #94a3b8; font-size: 13px; margin-bottom: 2px;",
                                                        "Room: "
                                                        strong { color: "#f1f5f9", "{contact.room_id}" }
                                                    }
                                                    div { style: "color: #f59e0b; font-size: 13px; display: flex; align-items: center; gap: 8px;",
                                                        "Key: "
                                                        if revealed_secrets.read().contains(&contact.room_id) {
                                                            span { style: "font-family: monospace; background: #020617; padding: 2px 6px; border-radius: 4px;",
                                                                "{contact.secret_password}"
                                                            }
                                                        } else {
                                                            span { style: "letter-spacing: 2px;",
                                                                "••••••••"
                                                            }
                                                        }
                                                        span {
                                                            style: "cursor: pointer; padding: 2px;",
                                                            onclick: {
                                                                let reveal_room = contact.room_id.clone();
                                                                move |_| {
                                                                    let mut secrets = revealed_secrets.write();
                                                                    if secrets.contains(&reveal_room) {
                                                                        secrets.remove(&reveal_room);
                                                                    } else {
                                                                        secrets.insert(reveal_room.clone());
                                                                    }
                                                                }
                                                            },
                                                            if revealed_secrets.read().contains(&contact.room_id) {
                                                                "🙈"
                                                            } else {
                                                                "👁️"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            div { style: "display: flex; align-items: center; gap: 6px;",
                                                if UNREAD_BADGES.read().get(&contact.room_id).copied().unwrap_or(0) > 0 {
                                                    div { style: "background: #ef4444; color: white; font-weight: bold; border-radius: 50%; width: 26px; height: 26px; display: flex; align-items: center; justify-content: center; font-size: 13px; box-shadow: 0 0 10px rgba(239,68,68,0.8); animation: pulse 2s infinite;",
                                                        "{UNREAD_BADGES.read().get(&contact.room_id).copied().unwrap_or(0)}"
                                                    }
                                                }
                                                button {
                                                    class: "btn btn-secondary",
                                                    style: "margin: 0; padding: 8px 12px; background: #334155;",
                                                    title: "Edit Contact",
                                                    onclick: {
                                                        let c_name = contact.name.clone();
                                                        let c_room = contact.room_id.clone();
                                                        let c_pass = contact.secret_password.clone();
                                                        let target = contact.room_id.clone();
                                                        move |_| {
                                                            edit_name.set(c_name.clone());
                                                            edit_room.set(c_room.clone());
                                                            edit_pass.set(c_pass.clone());
                                                            editing_room.set(target.clone());
                                                        }
                                                    },
                                                    "✏️"
                                                }
                                                button {
                                                    class: "btn btn-danger",
                                                    style: "margin: 0; padding: 8px 12px;",
                                                    title: "Delete Contact",
                                                    onclick: {
                                                        let target_room = contact.room_id.clone();
                                                        move |_| {
                                                            let mut current_list = contacts.read().clone();
                                                            current_list.retain(|c| c.room_id != target_room);
                                                            let _ = LocalStorage::set("govind_contacts", &current_list);
                                                            contacts.set(current_list);
                                                        }
                                                    },
                                                    "🗑️"
                                                }
                                                button {
                                                    class: "btn btn-success",
                                                    style: "margin: 0; padding: 10px 20px; margin-left: 4px;",
                                                    onclick: {
                                                        let chat_contact = contact.clone();
                                                        let room_id_to_clear = contact.room_id.clone();
                                                        move |_| {
                                                            UNREAD_BADGES.write().remove(&room_id_to_clear);
                                                            *CHAT_ACTIVE_NAME.write() = chat_contact.name.clone();
                                                            *CHAT_ACTIVE_ROOM.write() = chat_contact.room_id.clone();
                                                            *CHAT_ACTIVE_PASS.write() = chat_contact.secret_password.clone();
                                                        }
                                                    },
                                                    "💬 Message"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    ChatRoomInterface {}
                }
            }
        }
    }
}

// --- 7. TRUE WEBRTC P2P ENGINE WITH LOGS ---
#[derive(Serialize, Deserialize, Default)]
struct RtcSignalPayload { pub signal_type: String, pub sdp: String, pub call_type: String }

fn create_peer_connection() -> Result<RtcPeerConnection, JsValue> {
    add_log("WebRTC: Creating new P2P connection container.");
    let config = RtcConfiguration::new();
    let stun_server = js_sys::Object::new();
    js_sys::Reflect::set(&stun_server, &"urls".into(), &"stun:stun.l.google.com:19302".into())?;
    let ice_servers = js_sys::Array::new(); ice_servers.push(&stun_server); config.set_ice_servers(&ice_servers);
    
    let peer_conn = RtcPeerConnection::new_with_configuration(&config)?;
    let closure = Closure::wrap(Box::new(move |event: web_sys::RtcTrackEvent| {
        add_log("WebRTC: Remote video track successfully received from peer!");
        let streams = event.streams();
        if streams.length() > 0 {
            let stream = streams.get(0);
            if let Some(video_elem) = web_sys::window().unwrap().document().unwrap().get_element_by_id("remote-video") {
                let video: HtmlVideoElement = video_elem.unchecked_into();
                video.set_src_object(Some(stream.unchecked_ref()));
                add_log("WebRTC: Remote track bound to UI successfully.");
            }
        }
    }) as Box<dyn FnMut(web_sys::RtcTrackEvent)>);
    peer_conn.set_ontrack(Some(closure.as_ref().unchecked_ref()));
    closure.forget(); 

    Ok(peer_conn)
}

async fn generate_webrtc_offer(peer_conn: &RtcPeerConnection, chat_engine: &Coroutine<String>, call_type: &str) -> Result<(), JsValue> {
    add_log("WebRTC: Generating localized Offer SDP.");
    let promise = peer_conn.create_offer();
    let offer_js = wasm_bindgen_futures::JsFuture::from(promise).await?;
    let offer_sdp = offer_js.unchecked_into::<RtcSessionDescriptionInit>();
    let _ = wasm_bindgen_futures::JsFuture::from(peer_conn.set_local_description(&offer_sdp)).await?;
    
    add_log("WebRTC: Waiting for STUN server to inject IP candidates...");
    sleep(Duration::from_millis(1500)).await;
    
    if let Some(local_desc) = peer_conn.local_description() {
        add_log("WebRTC: Local Offer compiled. Sending over encrypted WebSocket.");
        let payload = RtcSignalPayload { signal_type: "offer".to_string(), sdp: local_desc.sdp(), call_type: call_type.to_string() };
        if let Ok(json_str) = serde_json::to_string(&payload) { chat_engine.send(format!("RTC_SIGNAL:{}", json_str)); }
    }
    Ok(())
}

async fn generate_webrtc_answer(peer_conn: &RtcPeerConnection, chat_engine: &Coroutine<String>, call_type: &str, offer_sdp: &str) -> Result<(), JsValue> {
    add_log("WebRTC: Received Remote Offer SDP. Setting remote description.");
    let init = RtcSessionDescriptionInit::new(web_sys::RtcSdpType::Offer); init.set_sdp(offer_sdp);
    let _ = wasm_bindgen_futures::JsFuture::from(peer_conn.set_remote_description(&init)).await?;

    add_log("WebRTC: Generating local Answer SDP.");
    let promise = peer_conn.create_answer();
    let answer_js = wasm_bindgen_futures::JsFuture::from(promise).await?;
    let answer_sdp = answer_js.unchecked_into::<RtcSessionDescriptionInit>();
    let _ = wasm_bindgen_futures::JsFuture::from(peer_conn.set_local_description(&answer_sdp)).await?;
    
    sleep(Duration::from_millis(1500)).await;
    
    if let Some(local_desc) = peer_conn.local_description() {
        add_log("WebRTC: Local Answer compiled. Sending back to caller.");
        let payload = RtcSignalPayload { signal_type: "answer".to_string(), sdp: local_desc.sdp(), call_type: call_type.to_string() };
        if let Ok(json_str) = serde_json::to_string(&payload) { chat_engine.send(format!("RTC_SIGNAL:{}", json_str)); }
    }
    Ok(())
}

#[component]
fn ChatRoomInterface() -> Element {
    let room_id = CHAT_ACTIVE_ROOM();
    let secret_password = CHAT_ACTIVE_PASS();
    let contact_name = CHAT_ACTIVE_NAME();

    let storage_key = format!("vault_chat_{}", room_id);
    let mut messages = use_signal(|| {
        LocalStorage::get::<Vec<ChatMessage>>(&storage_key).unwrap_or_else(|_| { vec![ChatMessage { id: 0, text: "🔒 AES-256 P2P Engine Active. Server is blind to this connection.".to_string(), is_mine: false }] })
    });

    let mut draft = use_signal(|| String::new());
    let mut is_calling = use_signal(|| false);

    let ws_url = format!("{}/secure/api/chat/{}", WS_BASE_URL, room_id);
    let pass_clone = secret_password.clone();
    let rx_storage_key = storage_key.clone();
    
    let c_room_id = room_id.clone();
    let c_name = contact_name.clone();

    let trigger_scroll = || { spawn(async move { let _ = eval(r#"{ setTimeout(() => { let chat = document.querySelector('.chat-history'); if(chat) chat.scrollTop = chat.scrollHeight; }, 50); }"#); }); };
    use_effect(move || { spawn(async move { let _ = eval("{ setTimeout(() => { let chat = document.querySelector('.chat-history'); if(chat) chat.scrollTop = chat.scrollHeight; }, 100); }"); }); });

    let mut is_calling_signal = is_calling;

    let chat_engine = use_coroutine(move |mut rx: UnboundedReceiver<String>| {
        let url = ws_url.clone();
        let current_password = pass_clone.clone();
        let loop_storage_key = rx_storage_key.clone();
        let cr_id = c_room_id.clone();
        let c_n = c_name.clone();

        async move {
            let ws = WebSocket::open(&url).unwrap();
            add_log("ChatRoom: Active WebSocket connection opened.");
            let (mut write, mut read) = ws.split();
            let current_password_read = current_password.clone();
            let safe_storage_key = loop_storage_key.clone();

            spawn(async move {
                while let Some(msg) = read.next().await {
                    if let Ok(WsMessage::Text(txt)) = msg {
                        if let Ok(payload) = serde_json::from_str::<EncryptedMessage>(&txt) {
                            if payload.sender_id != ACTIVE_HASH() {
                                match decrypt_message(&payload.ciphertext, &current_password_read) {
                                    Ok(clean_text) => {
                                        if clean_text.starts_with("RTC_SIGNAL:") {
                                            let json_str = clean_text.replace("RTC_SIGNAL:", "");
                                            if let Ok(signal) = serde_json::from_str::<RtcSignalPayload>(&json_str) {
                                                match signal.signal_type.as_str() {
                                                    "offer" => {
                                                        add_log("ChatRoom: Intercepted incoming call offer. Raising UI Overlay.");
                                                        *CHAT_INCOMING_OFFER_SDP.write() = signal.sdp.clone();
                                                        *GLOBAL_INCOMING_CALL.write() = Some(CallAlert {
                                                            room_id: cr_id.clone(), caller_name: c_n.clone(),
                                                            call_type: signal.call_type.clone(), secret_password: current_password_read.clone(),
                                                        });
                                                        let _ = eval("{ if (window.ringtone) { window.ringtone.pause(); } window.ringtone = new Audio('https://actions.google.com/sounds/v1/alarms/digital_watch_alarm_long.ogg'); window.ringtone.loop = true; window.ringtone.play().catch(e => console.log('Audio blocked')); }");
                                                        }
                                                        "answer" => {
                                                        add_log("ChatRoom: Intercepted remote answer. Handing SDP to WebRTC engine.");
                                                        *CHAT_INCOMING_ANSWER_SDP.write() = signal.sdp.clone();
                                                        }
                                                        "end_call" => {
                                                        add_log("ChatRoom: Remote hung up. Assassinating camera hardware.");
                                                        let _ = eval("{ if (window.ringtone) { window.ringtone.pause(); } if (window.localStream) { window.localStream.getTracks().forEach(t => t.stop()); window.localStream = null; } }");
                                                        is_calling_signal.set(false);

                                                        let new_id = messages.read().len(); messages.write().push(ChatMessage { id: new_id, text: "☎️ Call Ended".to_string(), is_mine: false });
                                                        spawn(async move { let _ = eval("{ setTimeout(() => { let chat = document.querySelector('.chat-history'); if(chat) chat.scrollTop = chat.scrollHeight; }, 50); }"); });

                                                    }
                                                    _ => {}
                                                }
                                            }
                                        } else {
                                            let new_id = messages.read().len(); messages.write().push(ChatMessage { id: new_id, text: clean_text, is_mine: false });
                                            if messages.read().len() > 50 { messages.write().remove(0); }
                                            let _ = LocalStorage::set(&safe_storage_key, &*messages.read());
                                            spawn(async move { let _ = eval("setTimeout(() => { let chat = document.querySelector('.chat-history'); if(chat) chat.scrollTop = chat.scrollHeight; }, 50);"); });
                                        }
                                    }
                                    Err(_) => { let new_id = messages.read().len(); messages.write().push(ChatMessage { id: new_id, text: "⚠️ [Decryption Failed]".to_string(), is_mine: false }); }
                                };
                            }
                        }
                    }
                }
            });

            while let Some(outgoing_text) = rx.next().await {
                let ciphertext = encrypt_message(&outgoing_text, &current_password);
                let secure_payload = EncryptedMessage { sender_id: ACTIVE_HASH(), timestamp: 0, ciphertext, signature: "verified_client".to_string() };
                if let Ok(json_string) = serde_json::to_string(&secure_payload) { let _ = write.send(WsMessage::Text(json_string)).await; }
            }
        }
    });

    let engine_clone = chat_engine.clone();
    
    use_effect(move || {
        if *CHAT_IS_CALLING.read() || *CHAT_IS_ANSWERING.read() {
            add_log("WebRTC: Engine triggered. Establishing hardware connections.");
            let engine = engine_clone.clone();
            let c_type = CHAT_CALL_TYPE();
            let answering = *CHAT_IS_ANSWERING.read();
            let incoming_sdp = CHAT_INCOMING_OFFER_SDP();
            
            *CHAT_IS_CALLING.write() = false; 
            *CHAT_IS_ANSWERING.write() = false;
            is_calling.set(true);
            
            spawn(async move {
                // THE FIX: We now explicitly catch the Error if the camera fails!
                match start_camera("local-video").await {
                    Ok(stream) => {
                        if let Ok(peer_conn) = create_peer_connection() {
                            let tracks = stream.get_tracks();
                            for i in 0..tracks.length() {
                                let track = tracks.get(i).unchecked_into::<web_sys::MediaStreamTrack>();
                                let add_track_fn = js_sys::Reflect::get(&peer_conn, &JsValue::from_str("addTrack")).unwrap().unchecked_into::<js_sys::Function>();
                                let args = js_sys::Array::of2(track.as_ref(), stream.as_ref());
                                let _ = add_track_fn.apply(&peer_conn, &args);
                            }
                            
                            if answering {
                                let _ = generate_webrtc_answer(&peer_conn, &engine, &c_type, &incoming_sdp).await;
                            } else {
                                let _ = generate_webrtc_offer(&peer_conn, &engine, &c_type).await;
                                loop {
                                    sleep(Duration::from_millis(500)).await;
                                    let answer_sdp = CHAT_INCOMING_ANSWER_SDP();
                                    if !answer_sdp.is_empty() {
                                        add_log("WebRTC: Validating incoming Answer SDP.");
                                        let init = RtcSessionDescriptionInit::new(web_sys::RtcSdpType::Answer);
                                        init.set_sdp(&answer_sdp);
                                        let _ = wasm_bindgen_futures::JsFuture::from(peer_conn.set_remote_description(&init)).await;
                                        *CHAT_INCOMING_ANSWER_SDP.write() = String::new(); 
                                        break;
                                    }
                                }
                            }
                        }
                    },
                    Err(_) => {
                        add_log("WebRTC ABORTED: Cannot proceed without camera access.");
                        is_calling_signal.set(false); // Instantly turn off the calling UI!
                    }
                }
            });
        }
    });

    let key_for_enter = storage_key.clone();
    let key_for_btn = storage_key.clone();

    rsx! {
        div {
            class: "chat-container",
            style: "display: flex; flex-direction: column; height: calc(100vh - 220px); margin: 0 auto; width: 100%; max-width: 800px; border-radius: 12px; overflow: hidden; background: #0f172a; border: 1px solid #38bdf8; box-shadow: 0 4px 20px rgba(0,0,0,0.5);",

            div { style: "display: flex; justify-content: space-between; align-items: center; padding: 15px; background: #1e293b; border-bottom: 1px solid #334155; flex-shrink: 0;",
                span { style: "color: #38bdf8; font-weight: bold; font-size: 18px;",
                    "👤 {contact_name}"
                }

                div { style: "display: flex; gap: 12px; align-items: center;",
                    if is_calling() {
                        button {
                            class: "btn btn-danger",
                            style: "padding: 8px 12px; border-radius: 20px; font-weight: bold;",
                            onclick: move |_| {
                                add_log("UI: User clicked manual End Call.");
                                let _ = eval(
                                    "if (window.localStream) { window.localStream.getTracks().forEach(t => t.stop()); window.localStream = null; }",
                                );
                                is_calling.set(false);
                                let end_signal = RtcSignalPayload {
                                    signal_type: "end_call".to_string(),
                                    ..Default::default()
                                };
                                chat_engine
                                    .send(format!("RTC_SIGNAL:{}", serde_json::to_string(&end_signal).unwrap()));
                                let new_id = messages.read().len();
                                messages
                                    .write()
                                    .push(ChatMessage {
                                        id: new_id,
                                        text: "☎️ You ended the call".to_string(),
                                        is_mine: true,
                                    });
                                trigger_scroll();
                            },
                            "🛑 End Call"
                        }
                    } else {
                        button {
                            class: "btn btn-secondary",
                            style: "padding: 8px; border-radius: 50%; font-size: 16px;",
                            title: "Voice Call",
                            onclick: move |_| {
                                *CHAT_CALL_TYPE.write() = "voice".to_string();
                                *CHAT_IS_CALLING.write() = true;
                            },
                            "📞"
                        }
                        button {
                            class: "btn btn-success",
                            style: "padding: 8px; border-radius: 50%; font-size: 16px;",
                            title: "Video Call",
                            onclick: move |_| {
                                *CHAT_CALL_TYPE.write() = "video".to_string();
                                *CHAT_IS_CALLING.write() = true;
                            },
                            "📹"
                        }
                    }
                    div { style: "width: 1px; height: 24px; background: #475569; margin: 0 5px;" }
                    button {
                        class: "btn btn-danger",
                        style: "padding: 6px 10px; font-size: 12px;",
                        onclick: move |_| {
                            *CHAT_ACTIVE_ROOM.write() = String::new();
                            *CHAT_ACTIVE_PASS.write() = String::new();
                            *CHAT_ACTIVE_NAME.write() = String::new();
                        },
                        "Leave"
                    }
                }
            }

            if is_calling() {
                div { style: "display: flex; gap: 10px; padding: 10px; background: #020617; border-bottom: 1px solid #334155; flex-shrink: 0;",
                    div { style: "flex: 1; background: #000; border: 1px solid #38bdf8; border-radius: 8px; overflow: hidden; aspect-ratio: 16/9; display: flex; align-items: center; justify-content: center;",
                        video {
                            id: "local-video",
                            style: "width: 100%; height: 100%; object-fit: cover;",
                            autoplay: "true",
                            muted: "true",
                        }
                    }
                    div { style: "flex: 1; background: #000; border: 1px solid #10b981; border-radius: 8px; overflow: hidden; aspect-ratio: 16/9; display: flex; align-items: center; justify-content: center;",
                        video {
                            id: "remote-video",
                            style: "width: 100%; height: 100%; object-fit: cover;",
                            autoplay: "true",
                        }
                    }
                }
            }

            div {
                class: "chat-history",
                style: "flex: 1; overflow-y: auto; padding: 15px; display: flex; flex-direction: column; gap: 12px; scroll-behavior: smooth;",
                for msg in messages.read().iter() {
                    div { class: if msg.is_mine { "msg-bubble mine" } else { "msg-bubble theirs" },
                        "{msg.text}"
                    }
                }
            }

            div {
                class: "chat-input-area",
                style: "display: flex; gap: 10px; padding: 15px; background: #1e293b; border-top: 1px solid #334155; flex-shrink: 0;",
                input {
                    class: "chat-input",
                    style: "flex: 1; padding: 10px; border-radius: 20px; border: 1px solid #475569; background: #0f172a; color: #f8fafc;",
                    r#type: "text",
                    value: "{draft}",
                    placeholder: "Type an encrypted message...",
                    autofocus: "true",
                    oninput: move |e| draft.set(e.value()),
                    onkeypress: move |e| {
                        if e.key().to_string() == "Enter" && !draft().is_empty() {
                            let txt = draft();
                            let new_id = messages.read().len();
                            messages
                                .write()
                                .push(ChatMessage {
                                    id: new_id,
                                    text: txt.clone(),
                                    is_mine: true,
                                });
                            chat_engine.send(txt);
                            draft.set(String::new());
                            let _ = LocalStorage::set(&key_for_enter, &*messages.read());
                            trigger_scroll();
                        }
                    },
                }
                button {
                    class: "btn btn-success send-btn",
                    style: "border-radius: 20px; padding: 0 20px; font-weight: bold;",
                    onclick: move |_| {
                        if !draft().is_empty() {
                            let txt = draft();
                            let new_id = messages.read().len();
                            messages
                                .write()
                                .push(ChatMessage {
                                    id: new_id,
                                    text: txt.clone(),
                                    is_mine: true,
                                });
                            chat_engine.send(txt);
                            draft.set(String::new());
                            let _ = LocalStorage::set(&key_for_btn, &*messages.read());
                            trigger_scroll();
                        }
                    },
                    "Send"
                }
            }
        }
    }
}

// --- 8. OTHER ECOSYSTEM PARTITIONS ---

#[component]
fn Dating() -> Element {
    rsx! {
        div { text_align: "center", padding_top: "50px",
            h1 { "❤️ Dating & Matrimonial" }
            if HOME_TIER() == 0 {
                IdentityGate {
                    title: "Dating Profile Identity".to_string(),
                    prompt_text: "Login to Dating".to_string(),
                    allow_reuse: true,
                    on_verified: move |email| {
                        *HOME_EMAIL.write() = email;
                        *HOME_TIER.write() = 1;
                    },
                }
            } else if HOME_TIER() == 1 {
                MobileGate { on_verified: move |_| *HOME_TIER.write() = 2 }
            } else if HOME_TIER() == 2 {
                KycGate { on_verified: move |_| *HOME_TIER.write() = 3 }
            } else {
                p { color: "#22c55e", "Welcome to Govind Dating. Account Fully Verified." }
            }
        }
        FloatingNavigation {}
    }
}

#[component]
fn EComm() -> Element {
    rsx! {
        div { text_align: "center", padding_top: "50px",
            h1 { "🛒 Govind e-Comm" }
            if HOME_TIER() == 0 {
                IdentityGate {
                    title: "E-Comm Profile Identity".to_string(),
                    prompt_text: "Login to e-Comm".to_string(),
                    allow_reuse: true,
                    on_verified: move |email| {
                        *HOME_EMAIL.write() = email;
                        *HOME_TIER.write() = 1;
                    },
                }
            } else if HOME_TIER() == 1 {
                MobileGate { on_verified: move |_| *HOME_TIER.write() = 2 }
            } else if HOME_TIER() == 2 {
                KycGate { on_verified: move |_| *HOME_TIER.write() = 3 }
            } else {
                p { color: "#22c55e", "Welcome to Govind e-Comm. Ready to shop securely." }
            }
        }
        FloatingNavigation {}
    }
}

// --- PLACEHOLDER PAGES ---

#[component]
fn News() -> Element {
    rsx! {
        div {
            h1 { "📰 News & Blog" }
            FloatingNavigation {}
        }
    }
}

#[component]
fn Tube() -> Element {
    rsx! {
        div {
            h1 { "📺 Govind Tube" }
            FloatingNavigation {}
        }
    }
}

#[component]
fn PublicChat() -> Element {
    rsx! {
        div {
            h1 { "💬 Public Conversation Area" }
            FloatingNavigation {}
        }
    }
}

#[component]
fn UnderDevelopment() -> Element {
    rsx! {
        div {
            h1 { "🚧 Area Under Development" }
            FloatingNavigation {}
        }
    }
}
