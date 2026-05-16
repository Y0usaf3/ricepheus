use axum::Json;
use axum::extract::{FromRef, State};
use axum::response::Html;
use axum::response::IntoResponse;
use axum::{
    Extension, Router,
    response::Redirect,
    routing::{get, post},
};
use axum_extra::extract::OptionalQuery;
use axum_extra::extract::cookie::SameSite;
use axum_extra::extract::cookie::{Cookie, Key, PrivateCookieJar};
use reqwest::StatusCode;
use slack_morphism::prelude::*;
use std::collections::{HashMap, HashSet};
use std::env;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tera::{Context, Tera};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;

const HT_CLIENT_ID: LazyLock<String> = LazyLock::new(|| env::var("CLIENT_ID").unwrap());
const HT_CLIENT_SECRET: LazyLock<String> = LazyLock::new(|| env::var("CLIENT_SECRET").unwrap());
const HT_REDIRECT_URI: LazyLock<String> = LazyLock::new(|| env::var("REDIRECT_URI").unwrap());
const SLACK_TOKEN: LazyLock<String> = LazyLock::new(|| env::var("SLACK_TOKEN").unwrap());
const START_DATE: LazyLock<String> =
    LazyLock::new(|| env::var("START_DATE").unwrap_or_else(|_| "2025-12-22T00:00:00Z".to_string()));
const OWNER: LazyLock<String> = LazyLock::new(|| env::var("OWNER").expect("OWNER env var not set"));

#[derive(Debug, Default, Clone)]
struct VotingSession {
    is_active: bool,
    participants: HashSet<String>,
    waiting_pool: Vec<String>,
    current_candidate: Option<String>,
    votes: HashMap<String, usize>,
    voted_users: HashSet<String>,
}

#[derive(Clone)]
struct AppState {
    key: Key,
    submitted_users: Arc<RwLock<HashSet<u64>>>,
    slack_token: SlackApiToken,
    voting_session: Arc<RwLock<VotingSession>>,
}

#[derive(serde::Serialize, Debug)]
struct CodeExchange<'a> {
    client_id: String,
    client_secret: String,
    code: String,
    redirect_uri: String,
    grant_type: &'a str,
}

#[derive(serde::Deserialize, Debug)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u32,
    scope: String,
    created_at: u32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct UserResponse {
    pub id: u64,
    pub emails: Vec<String>,
    pub slack_id: String,
    pub github_username: String,
    pub trust_factor: TrustFactor,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TrustFactor {
    pub trust_level: String,
    pub trust_value: i32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ProjectsResponse {
    pub projects: Vec<Project>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Project {
    pub name: String,
    pub total_seconds: u64,
    pub most_recent_heartbeat: String,
    pub languages: Vec<String>,
    pub archived: bool,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.key.clone()
    }
}

async fn send_slack_message(
    token: &SlackApiToken,
    channel: &str,
    text: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client =
        SlackClient::new(SlackClientHyperConnector::new().expect("Failed to create Slack client"));

    let session = client.open_session(token);
    let message = SlackMessageContent {
        text: Some(text),
        blocks: None,
        attachments: None,
        upload: None,
        files: None,
        reactions: None,
        metadata: None,
    };

    let post_chat_req = SlackApiChatPostMessageRequest::new(channel.into(), message);
    session.chat_post_message(&post_chat_req).await?;
    Ok(())
}

async fn post_to_master_thread(token: SlackApiToken, channel_id: SlackChannelId, text: String) {
    let target_thread_ts = env::var("TARGET_THREAD_TS").unwrap_or_default();
    let client = SlackClient::new(SlackClientHyperConnector::new().unwrap());
    let session = client.open_session(&token);
    let msg = SlackMessageContent::new().with_text(text);
    let mut req = SlackApiChatPostMessageRequest::new(channel_id, msg);
    if !target_thread_ts.is_empty() {
        req = req.with_thread_ts(target_thread_ts.into());
    }
    let _ = session.chat_post_message(&req).await;
}

fn parse_slack_mention(text: &str) -> Option<String> {
    dbg!(text);
    let text = text.trim();
    if text.starts_with("<@") && text.contains('>') {
        let end_idx = text.find('>').unwrap();
        let internal = &text[2..end_idx];
        if let Some(pipe_idx) = internal.find('|') {
            Some(internal[..pipe_idx].to_string())
        } else {
            Some(internal.to_string())
        }
    } else {
        None
    }
}

#[derive(serde::Deserialize, Debug)]
struct Callback {
    code: String,
}

#[derive(serde::Deserialize, Debug)]
struct FormData {
    #[serde(default, deserialize_with = "deserialize_selected_projects")]
    selected_projects: Vec<String>,
}

fn deserialize_selected_projects<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or a sequence of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(vec![value.to_owned()])
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_any(StringOrVec)
}

#[tokio::main]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls default crypto provider");
    tracing_subscriber::fmt::init();

    let token_value: SlackApiTokenValue = SLACK_TOKEN.as_str().into();
    let token: SlackApiToken = SlackApiToken::new(token_value);

    let token_clone = token.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let trimmed = line.trim();
            if !trimmed.is_empty()
                && let Err(e) =
                    send_slack_message(&token_clone, "#riceathon", trimmed.to_string()).await
            {
                eprintln!("Error sending message from stdin: {}", e);
            }
        }
    });

    let token_clone = token.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(content) = tokio::fs::read_to_string("msg.txt").await {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    if let Err(e) =
                        send_slack_message(&token_clone, "#riceathon", trimmed.to_string()).await
                    {
                        eprintln!("Error sending message from msg.txt: {}", e);
                    } else {
                        let _ = tokio::fs::write("msg.txt", "").await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });

    let tera = Tera::new("templates/**/*.html").expect("Failed to initialize Tera");

    let mut submitted_users = HashSet::new();
    if let Ok(content) = std::fs::read_to_string("submitted_users.txt") {
        for line in content.lines() {
            if let Ok(id) = line.parse::<u64>() {
                submitted_users.insert(id);
            }
        }
    }

    let state = AppState {
        key: Key::generate(),
        submitted_users: Arc::new(RwLock::new(submitted_users)),
        slack_token: token,
        voting_session: Arc::new(RwLock::new(VotingSession::default())),
    };
    let app = Router::new()
        .route("/", get(root))
        .route("/err", get(err))
        .route("/submit", post(submit))
        // bot slash cmds
        .route("/add_me", post(handle_add_me))
        .route("/start", post(handle_start))
        .route("/stop", post(handle_stop))
        .route("/current", post(handle_current))
        .route("/next", post(handle_next))
        .route("/vote", post(handle_vote))
        .layer(axum::Extension(tera))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    let _ = axum::serve(listener, app).await;
}

async fn submit(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    axum::Form(form): axum::Form<Vec<(String, String)>>,
) -> Result<&'static str, Redirect> {
    let token = jar
        .get("token")
        .map(|c| c.value().to_string())
        .ok_or_else(|| Redirect::to("/err"))?;

    let client = reqwest::Client::new();

    let user = client
        .get("https://hackatime.hackclub.com/api/v1/authenticated/me")
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|_| Redirect::to("/err"))?
        .json::<UserResponse>()
        .await
        .map_err(|_| Redirect::to("/err"))?;

    {
        let submitted = state.submitted_users.read().await;
        if submitted.contains(&user.id) {
            return Ok(r#"

⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣠⡶⠟⣛⣽⣿⣧⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣠⡤⠤⢤⡴⠛⠁⠀⣴⠋⠱⣿⣿⡆⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣴⣶⠶⠶⠶⣤⣤⡶⠶⠾⠋⠀⠀⠈⠀⠀⠀⢰⣧⣀⣰⣟⠙⣷⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣿⡟⠳⣄⡴⠋⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠙⢾⣿⣛⡀⣿⠀⠀⠀⠀⠀KID UR CAUGHT!!⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠛⣿⣏⣠⡟⠁⠀⠀⢀⣴⡀⠀⡀⠀⣤⣄⠀⢤⣀⠀⠈⠁⠈⠳⣿⡀⠀⠀⠀⠀U THINK U CAN SUBMIT TWO TIMES IN A ROW ?!⠀⠀⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠿⣿⡟⣼⣷⣴⣶⣿⠁⢹⡄⠻⣶⣿⣯⣀⡀⣿⣷⠀⠀⠀⢀⡈⢿⣄⠀⠀⠀DONT THINK U CAN DO THAT WITH ME >:3⠀⠀⠀
⠀⠀⠀⠀⠀⠀⣀⣀⣀⣀⡀⡿⠋⢻⠻⠶⣤⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣹⣿⣿⣿⣿⣼⠹⣦⣧⣿⣆⠙⠛⣯⡻⠿⣆⠈⠁⠀⠀⠈⠙⣮⡿⣦⣀⠀⠀⠀⠀
⠀⠀⠀⠀⢀⣾⣿⣥⣴⣭⣿⣷⣤⣼⢴⣒⡮⣽⡻⢦⡄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⡟⣼⣿⣿⠉⢿⣇⠙⠋⢹⣯⣄⠀⣘⣿⣦⣼⣷⣤⠀⠀⣀⠀⠈⢿⡦⢿⣷⡦⠀⠀
⠀⠀⠀⠀⣾⢹⣿⣿⣿⣿⣿⡿⢿⣿⣿⣿⣿⣾⡿⡟⠻⢶⣤⣀⠀⠀⠀⠀⠀⠀⠀⠀⣾⠟⣿⣿⣇⣀⣼⣿⣦⡀⠀⣿⣿⣿⣿⡏⠁⠀⠀⠙⢷⠀⠙⡆⠀⠘⣷⠀⠀⠀⠀⠀
⠀⣀⣤⠤⠿⠸⣿⣿⣿⣿⡿⠁⣿⣿⣿⣿⣿⣿⡇⣿⠃⠰⠀⠉⡛⠳⠶⣤⣀⣀⠀⠀⠀⢰⣿⣿⠿⠛⣿⣿⠻⢿⣶⡿⠋⢿⣿⡧⠀⠀⠀⢀⡾⠀⠀⢻⢦⣄⣻⣧⠀⠀⠀⠀
⣼⣿⣿⣿⣿⣦⡈⠙⠛⠉⠀⠀⠘⣿⣿⣿⣿⡿⣵⣃⡀⠀⠀⠀⠀⠀⠀⠒⠿⣿⣿⣶⣤⣼⡏⢻⣄⠀⢻⣿⠀⢀⡿⠳⣄⣈⣛⣃⣀⣤⠶⢿⡄⠀⠀⢸⣼⣯⠛⠛⠛⠀⠀⠀
⣿⣿⣿⣿⣿⣿⡇⣠⣴⣶⣶⣶⣦⡀⠉⠉⠁⣈⣭⣍⣙⢷⣶⠶⢶⣦⣤⣄⣀⣀⠀⠉⠙⠛⠿⢿⣿⣷⣶⣗⢺⣏⣰⣦⣤⣽⠟⠉⠉⠀⠀⣸⢿⣶⣄⣸⡏⠛⠓⠀⠀⠀⠀⠀
⣿⣿⣿⣿⣿⡿⣿⣿⣿⣿⣿⣿⣿⣿⣆⠀⣾⣿⣿⣿⣿⣷⣼⣇⠀⠀⠀⠀⠈⠉⠉⠛⠛⠷⠶⠶⠤⣭⣝⣿⣿⣿⣷⣯⣉⣹⡇⠀⣀⣠⡾⢻⣿⣿⡍⠛⠷⠀⠀⠀⠀⠀⠀⠀
⠙⠿⣿⣿⣯⠄⡏⣿⣿⣿⣿⣿⣿⣿⣿⠘⣿⣿⣿⣿⣿⣿⣇⡿⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⡀⠀⠀⠉⣩⡿⠛⢿⣿⣿⣶⣟⣩⣿⠗⠈⠀⠈⣿⠀⠀⠀⠀⠀⠀⠀⠀⠀
⠀⠀⠀⠀⢸⣇⣿⣿⣿⣿⣿⣿⣿⣿⡿⢠⣿⣿⣿⣿⡿⢏⣾⠓⠀⢀⣀⣀⣀⣠⣤⣤⣤⣴⣶⣶⣷⡶⠶⠾⠛⠀⠀⠀⠹⡏⠻⢿⣯⣾⣯⣀⠀⠀⣿⠀⣠⡶⠶⠾⣷⣦⡀⠀
⠀⠀⠀⠀⠈⢻⣮⡿⣿⣿⣿⣿⣿⠟⢁⣾⣙⣿⣿⣶⣾⣟⣛⣿⣭⠭⠿⠶⠾⠛⠛⠛⠉⠉⠁⢸⡁⣀⠀⠀⠀⠀⠀⠀⠀⡇⠀⠀⠈⠻⣿⡟⠷⣰⣿⠀⠻⠷⢤⣤⣀⠙⣧⡀
⠀⠀⠀⠀⠀⠀⠙⠻⣮⣍⣉⣩⣥⡶⠿⠛⠛⠛⠛⠋⠉⠉⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠘⣿⣽⣗⡀⠀⠀⠀⢀⣾⡇⠀⠀⠀⠀⠈⠃⣰⢿⡇⠀⠀⠀⠀⠈⠻⣇⠘⣧
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠉⠉⠉⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠛⠿⠿⢤⡶⣶⣿⣿⣷⣶⣤⡤⠶⠶⠞⠋⣾⠀⠀⠀⠀⠀⠀⠀⢻⠀⢻⣾
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣰⠏⣧⣀⣀⣀⣀⡀⠀⠀⠀⠠⣿⡀⠀⠀⠀⠀⠀⠀⣼⠀⣸⣿
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢸⣿⣤⣿⣭⡉⠉⠙⠛⣃⣠⣤⣶⣿⣧⡀⠀⠀⠀⣠⡼⠃⢠⡟
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣿⣿⣿⣿⠿⠛⠛⠛⠻⠿⣿⡿⠲⣿⣿⣝⣛⠚⠋⠉⣀⣴⠟⠀
⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣾⣿⣿⣿⡇⠀⠀⠀⠀⠀⠀⠈⠁⠀⠸⢻⣿⡛⠛⠛⠋⠉⠀⠀⠀
                "#);
        }
    }

    let selected_project_names: Vec<String> = form
        .into_iter()
        .filter(|(k, _)| k == "selected_projects")
        .map(|(_, v)| v)
        .collect();

    let projects_response = client
        .get("https://hackatime.hackclub.com/api/v1/authenticated/projects")
        .query(&[("start", START_DATE.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .map_err(|_| Redirect::to("/err"))?
        .json::<ProjectsResponse>()
        .await
        .map_err(|_| Redirect::to("/err"))?;

    let selected_projects: Vec<Project> = projects_response
        .projects
        .into_iter()
        .filter(|p| selected_project_names.contains(&p.name))
        .collect();

    let mut full_hours: u64 = 0;
    let mut full_minutes: u64 = 0;

    let project_details: Vec<String> = selected_projects
        .iter()
        .map(|p| {
            let hours = p.total_seconds / 3600;
            let minutes = (p.total_seconds % 3600) / 60;
            full_hours += hours;
            full_minutes += minutes;
            full_hours += full_minutes / 60;
            full_minutes %= 60;

            let name_lower = p.name.to_lowercase();
            let custom_msg = if name_lower.contains("nix") || name_lower.contains("nixos") {
                "woah nix :parrot-nix: !"
            } else if name_lower.contains("arch") {
                "nice config btw! :femboy-arch: "
            } else if name_lower.contains("sans") {
                "WAIT! is that sand :sans: "
            } else {
                ""
            };

            format!("→ *{}* ({}h {}m) {custom_msg}", p.name, hours, minutes)
        })
        .collect();

    let extra_msg = if full_hours > 67 {
        "\nWOW! great job!!! thats a lot of socks /silly"
    } else {
        "\nnice work!"
    };

    let total_h = if selected_projects.len() != 1 {
        format!("a total of {full_hours}h{full_minutes}m!")
    } else {
        "".to_string()
    };

    let message_text = format!(
        "<@{}> submitted their rice! :boykisser-dance:\n{}\n{total_h} {extra_msg}",
        user.slack_id,
        project_details.join("\n"),
    );

    let client = SlackClient::new(
        SlackClientHyperConnector::new()
            .ok()
            .ok_or(Redirect::to("/err"))?,
    );
    let token_value: SlackApiTokenValue = SLACK_TOKEN.as_str().into();
    let token: SlackApiToken = SlackApiToken::new(token_value);
    let session = client.open_session(&token);

    let message = SlackMessageContent {
        text: Some(message_text),
        blocks: None,
        attachments: None,
        upload: None,
        files: None,
        reactions: None,
        metadata: None,
    };

    let post_chat_req = SlackApiChatPostMessageRequest::new("#riceathon".into(), message);

    session
        .chat_post_message(&post_chat_req)
        .await
        .ok()
        .ok_or(Redirect::to("/err"))?;

    {
        let mut submitted = state.submitted_users.write().await;
        if submitted.insert(user.id) {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("submitted_users.txt")
                .await
                .map_err(|_| Redirect::to("/err"))?;
            file.write_all(format!("{}\n", user.id).as_bytes())
                .await
                .map_err(|_| Redirect::to("/err"))?;
        }
    }

    Ok(r#"
                       ⠀⠀⠀⠀⢠⡶⠚⢷⣤⡀⠀⠀⠀⠀⠀⣲⡶⠛⠻⣆⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀
                       ⠀⠀⠀⢠⡿⠁⠀⠀⠙⣷⣄⠀⢀⣴⡟⠁⠀⠀⢷⢹⡆⠀⠀⠀⠀⠀⠀⠀⠀⠀
     Thank you         ⠀⠀⠀⣾⠃⠀⠠⠶⠚⠛⠛⠛⠛⠋⠀⠀⣀⡀⢸⠈⣿⠀⠀⠀⠀⠀⠀⠀⠀⠀
     For Submitting !  ⠀⠀⢸⣏⡔⠋⠀⠀⠀⠀⠀⠀⠀⠀⠀⠚⠉⠉⣿⠀⢹⠀⠀⠀⠀⠀⠀⠀⠀⠀
                       ⠀⠀⢾⠏⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠸⠀⢸⡇⠀⠀⠀⠀⠀⠀⠀⠀
                       ⠀⢠⣿⢠⣶⡆⠀⠀⠀⠀⣀⣀⠀⠀⠀⠀⠀⠀⠀⠀⢸⡇⠀⠀⠀⠀⠀⠀⠀⠀
                       ⢒⡾⠁⠘⠟⠁⠀⠀⠀⠀⣿⣿⡆⠀⠀⠀⠀⠀⠀⠀⢸⡇⠀⠀⠀⠀⠀⠀⠀⠀
                       ⠉⣧⠀⠀⠀⠀⠃⠀⠀⠀⠈⠉⠠⣍⠀⠀⠀⠀⠀⠀⣸⡇⢀⣤⠶⠛⠛⠻⢦⣄
                       ⠀⠸⣧⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣰⡟⣴⠟⠁⠀⠀⠀⠀⠀⢻
                       ⠀⠀⠀⠛⣷⡦⠀⠀⠀⠀⠀⠀⠀⠀⣀⣀⣤⡴⠞⠋⢠⡟⠀⠀⠀⠀⠀⠀⢀⡾
                       ⠀⠀⠀⢰⡿⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠉⠳⣤⡀⢸⠃⠀⠀⠀⠀⢠⡶⠟⠁
                       ⠀⠀⠀⣸⠇⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠘⢷⣹⡄⠀⠀⠀⠀⣼⠀⠀⠀
                       ⠀⠀⠀⣿⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⢿⣇⠀⠀⠀⠀⢹⡄⠀⠀
                       ⠀⠀⠀⢸⡀⢀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⣿⡄⠀⠀⠀⠈⣧⠀⠀
                       ⠀⠀⠀⢸⡇⠘⡇⠀⠀⠀⠀⠀⠀⠀⣀⠀⠀⠀⠀⠀⠀⢸⣿⠀⠀⠀⠀⢹⡇⠀
                       ⠀⠀⠀⢸⡇⠀⠙⠀⠀⠀⠀⠀⢠⠞⠁⠀⠀⠀⠀⠀⠀⠀⣿⠇⠀⠀⠀⢸⡇⠀
                       ⠀⠀⠀⢸⡇⠀⢸⡆⠀⠀⠀⠀⣟⠀⠀⠀⠀⠀⠀⠀⠀⠀⠛⠀⠀⠀⠀⣸⠇⠀
                       ⠀⠀⠀⢸⣿⠀⠀⡇⠀⠀⠀⠀⣿⡀⠀⠀⠀⠀⠀⠀⠀⢀⡇⠀⠀⢀⣴⡟⠁⠀
                       ⠀⠀⠀⠘⠿⠶⢶⢧⣦⣦⡴⢾⣥⣽⣤⣤⣤⣤⣤⣤⡴⣯⡤⠴⠶⠛⠋⠀⠀⠀
        "#)
}

async fn root(
    OptionalQuery(params): OptionalQuery<Callback>,
    Extension(tera): Extension<Tera>,
    jar: PrivateCookieJar,
) -> Result<(PrivateCookieJar, Html<String>), Redirect> {
    match params {
        Some(Callback { code }) => {
            let client = reqwest::Client::new();

            let exchange_request = CodeExchange {
                client_id: HT_CLIENT_ID.to_string(),
                client_secret: HT_CLIENT_SECRET.to_string(),
                code,
                redirect_uri: HT_REDIRECT_URI.to_string(),
                grant_type: "authorization_code",
            };

            let response = client
                .post("https://hackatime.hackclub.com/oauth/token")
                .json(&exchange_request)
                .send()
                .await;

            match response {
                Ok(res) => {
                    if res.status().is_success() {
                        let token_data = res.json::<TokenResponse>().await;
                        match token_data {
                            Ok(token) => {
                                let cookie = Cookie::build(("token", token.access_token.clone()))
                                    .path("/")
                                    .secure(true)
                                    .http_only(true)
                                    .same_site(SameSite::Lax)
                                    .max_age(time::Duration::minutes(10))
                                    .build();
                                let jar = jar.add(cookie);

                                let user = client
                                    .get("https://hackatime.hackclub.com/api/v1/authenticated/me")
                                    .bearer_auth(&token.access_token)
                                    .send()
                                    .await
                                    .map_err(|_| Redirect::to("/err"))?
                                    .json::<UserResponse>()
                                    .await
                                    .map_err(|_| Redirect::to("/err"))?;

                                let projects = client
                                    .get("https://hackatime.hackclub.com/api/v1/authenticated/projects")
                                    .query(&[("start", START_DATE.as_str())])
                                    .bearer_auth(&token.access_token)
                                    .send()
                                    .await
                                    .map_err(|_| Redirect::to("/err"))?
                                    .json::<ProjectsResponse>()
                                    .await
                                    .map_err(|_| Redirect::to("/err"))?;

                                let mut context = Context::new();
                                context.insert("github_username", &user.github_username);
                                context.insert("projects", &projects.projects);

                                let rendered = tera.render("main.html", &context).unwrap();
                                Ok((jar, Html(rendered)))
                            }
                            Err(_) => Err(Redirect::to("/err")),
                        }
                    } else {
                        Err(Redirect::to("/err"))
                    }
                }
                Err(_) => Err(Redirect::to("/err")),
            }
        }
        _ => Err(Redirect::to(&format!(
            "https://hackatime.hackclub.com/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&scope=profile+read",
            HT_CLIENT_ID.as_str(),
            HT_REDIRECT_URI.as_str()
        ))),
    }
}

async fn err() -> &'static str {
    r#"           _______
　　　　　 /  ＞　　フ This cat is sad cuz it doesnt know what made you come here..
　　　　　| 　_　 _ l     Would you pat the cat?
　 　　　／` ミ＿xノ  
　　 　 /　　　 　 |
　　　 /　 ヽ　　 ﾉ
　 　 │　　|　|　|
　／￣|　　 |　|　|
　| (￣ヽ＿_ヽ_)__)
　＼二つ

"#
}

async fn handle_start(
    State(state): State<AppState>,
    axum::Form(event): axum::Form<SlackCommandEvent>,
) -> Result<StatusCode, Json<SlackCommandEventResponse>> {
    if event.user_id.to_string() != *OWNER {
        let err_content = SlackMessageContent::new()
            .with_text("ONLY YOUSAFE CAN START THE VOTING HUDDLE \nYOU NOT SAFE /silly".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    let mut session = state.voting_session.write().await;

    *session = VotingSession {
        is_active: true,
        ..Default::default()
    };

    let slack_token = state.slack_token.clone();
    let channel_id = event.channel_id.clone();

    tokio::spawn(async move {
        let msg = format!(
            ":yay: ayy, just started it, now ppl shall add themselves using `/add_me` so they can participate >:3\nyumm i wonder the rices im going to eat /silly",
        );
        post_to_master_thread(slack_token, channel_id, msg).await;
    });

    Ok(StatusCode::OK)
}

async fn handle_add_me(
    State(state): State<AppState>,
    axum::Form(event): axum::Form<SlackCommandEvent>,
) -> Result<StatusCode, Json<SlackCommandEventResponse>> {
    let mut session = state.voting_session.write().await;

    if !session.is_active {
        let err_content = SlackMessageContent::new().with_text("Nuh uh".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    let caller_id = event.user_id.to_string();
    if session.participants.contains(&caller_id) {
        let err_content =
            SlackMessageContent::new().with_text("Your already in the list silly!".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    session.participants.insert(caller_id.clone());
    session.waiting_pool.push(caller_id.clone());

    let slack_token = state.slack_token.clone();
    let channel_id = event.channel_id.clone();
    let total_count = session.participants.len();

    tokio::spawn(async move {
        let msg = format!(
            ":wavey: <@{caller_id}> joined the voting participant pool! now there is *{total_count}* participants in the list"
        );
        post_to_master_thread(slack_token, channel_id, msg).await;
    });

    Ok(StatusCode::OK)
}

async fn handle_next(
    State(state): State<AppState>,
    axum::Form(event): axum::Form<SlackCommandEvent>,
) -> Result<StatusCode, Json<SlackCommandEventResponse>> {
    if event.user_id.to_string() != *OWNER {
        let err_content =
            SlackMessageContent::new().with_text("Nuh uh, only the YOUSAFE can use this".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    let mut session = state.voting_session.write().await;
    if !session.is_active {
        let err_content = SlackMessageContent::new().with_text("Nuh uh".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    if session.waiting_pool.is_empty() {
        let slack_token = state.slack_token.clone();
        let channel_id = event.channel_id.clone();
        tokio::spawn(async move {
            post_to_master_thread(
                slack_token,
                channel_id,
                "No more participants in the list! u can run `/stop` to show the results".into(),
            )
            .await;
        });
        let ack = SlackMessageContent::new()
            .with_text("The participant pool is empty. :pensive-wobble:".into());
        return Err(Json(
            SlackCommandEventResponse::new(ack)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    // Fixed the potential rand compilation error using random_range
    let random_idx = rand::random_range(..session.waiting_pool.len());
    let chosen_one = session.waiting_pool.remove(random_idx);

    session.current_candidate = Some(chosen_one.clone());

    let slack_token = state.slack_token.clone();
    let channel_id = event.channel_id.clone();

    tokio::spawn(async move {
        let msg = format!("Now time for <@{chosen_one}> to show us their rice :yay: !");
        post_to_master_thread(slack_token, channel_id, msg).await;
    });

    Ok(StatusCode::OK)
}

async fn handle_vote(
    State(state): State<AppState>,
    axum::Form(event): axum::Form<SlackCommandEvent>,
) -> Result<StatusCode, Json<SlackCommandEventResponse>> {
    dbg!(&event);
    let mut session = state.voting_session.write().await;
    if !session.is_active {
        let err_content = SlackMessageContent::new().with_text("Nuh uh".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    let voter_id = event.user_id.to_string();

    let target_text = event.text.clone().unwrap_or_default();
    let target_id = match parse_slack_mention(&target_text) {
        Some(id) => id,
        None => {
            let err_content = SlackMessageContent::new()
                .with_text("format error ? please vote that way `/vote @user`".into());
            return Err(Json(
                SlackCommandEventResponse::new(err_content)
                    .with_response_type(SlackMessageResponseType::Ephemeral),
            ));
        }
    };

    if voter_id == target_id {
        let err_content = SlackMessageContent::new()
            .with_text("pfffff lmao what, what r u trying todo? u silly".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    if session.voted_users.contains(&voter_id) {
        let err_content =
            SlackMessageContent::new().with_text("You can only vote for one person".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    if !session.participants.contains(&target_id) {
        let err_content =
            SlackMessageContent::new().with_text("Huh ? the user is not in the list ??".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    session.voted_users.insert(voter_id.clone());
    *session.votes.entry(target_id.clone()).or_insert(0) += 1;

    let slack_token = state.slack_token.clone();
    let channel_id = event.channel_id.clone();

    tokio::spawn(async move {
        let log_msg = format!("looks like this rice impressed you! +1 point for them :happi:");
        post_to_master_thread(slack_token, channel_id, log_msg).await;
    });

    Ok(StatusCode::OK)
}

async fn handle_current(
    State(state): State<AppState>,
    axum::Form(event): axum::Form<SlackCommandEvent>,
) -> Result<StatusCode, Json<SlackCommandEventResponse>> {
    let session = state.voting_session.read().await;
    if !session.is_active {
        let err_content = SlackMessageContent::new().with_text("umm, nop".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    let slack_token = state.slack_token.clone();
    let channel_id = event.channel_id.clone();
    let response_text = match &session.current_candidate {
        Some(candidate) => format!("right now, <@{candidate}> is showing us their rice :3"),
        None => "no one showing their rice ?? YOUSAFE must call `/next` to choose a participant"
            .to_string(),
    };

    tokio::spawn(async move {
        post_to_master_thread(slack_token, channel_id, response_text).await;
    });

    Ok(StatusCode::OK)
}

async fn handle_stop(
    State(state): State<AppState>,
    axum::Form(event): axum::Form<SlackCommandEvent>,
) -> Result<StatusCode, Json<SlackCommandEventResponse>> {
    if event.user_id.to_string() != *OWNER {
        let err_content = SlackMessageContent::new().with_text("only YOUSAFE can stop this".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    let mut session = state.voting_session.write().await;
    if !session.is_active {
        let err_content = SlackMessageContent::new().with_text("Nuh uh".into());
        return Err(Json(
            SlackCommandEventResponse::new(err_content)
                .with_response_type(SlackMessageResponseType::Ephemeral),
        ));
    }

    let mut leaderboard_text =
        String::from("*:ultrafastparrot: final voting Leaderboard scores !* \n");
    let mut highest_votes = 0;
    let mut winners = Vec::new();

    if session.participants.is_empty() {
        leaderboard_text.push_str("_No registered participants joined this round._");
    } else {
        for user in &session.participants {
            let score = session.votes.get(user).cloned().unwrap_or(0);
            let upvote = if score > 9 {
                ":super-mega-upvote:"
            } else {
                ":upvote:"
            };
            leaderboard_text.push_str(&format!("- <@{user}>: *{score}*{upvote} votes\n"));

            if score > highest_votes {
                highest_votes = score;
                winners = vec![user.clone()];
            } else if score == highest_votes && score > 0 {
                winners.push(user.clone());
            }
        }
    }

    let final_announcement = if highest_votes == 0 {
        format!("{leaderboard_text}\nWAIT WHAT, NO WINNER WTH?!? there must be a mistake..")
    } else if winners.len() == 1 {
        format!(
            "{leaderboard_text}\n* ANNNNnnndddd congratulations to our winner <@{}> with {} votes!* :yay:",
            winners[0], highest_votes
        )
    } else {
        let ties: Vec<String> = winners.iter().map(|w| format!("<@{w}>")).collect();
        format!(
            "{leaderboard_text}\nHow is that possible, {} got *{} votes!*\nYOUSAFE i let you say the rest :p",
            ties.join(", "),
            highest_votes
        )
    };

    session.is_active = false;

    let slack_token = state.slack_token.clone();
    let channel_id = event.channel_id.clone();

    tokio::spawn(async move {
        post_to_master_thread(slack_token, channel_id, final_announcement).await;
    });

    Ok(StatusCode::OK)
}
