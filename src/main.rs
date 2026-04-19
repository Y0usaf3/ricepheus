use axum::extract::{FromRef, State};
use axum::response::Html;
use axum::{
    Extension, Router,
    response::Redirect,
    routing::{get, post},
};
use axum_extra::extract::OptionalQuery;
use axum_extra::extract::cookie::SameSite;
use axum_extra::extract::cookie::{Cookie, Key, PrivateCookieJar};
use slack_morphism::prelude::*;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tera::{Context, Tera};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;

const HT_CLIENT_ID: LazyLock<String> = LazyLock::new(|| std::env::var("CLIENT_ID").unwrap());
const HT_CLIENT_SECRET: LazyLock<String> =
    LazyLock::new(|| std::env::var("CLIENT_SECRET").unwrap());
const HT_REDIRECT_URI: LazyLock<String> = LazyLock::new(|| std::env::var("REDIRECT_URI").unwrap());
const SLACK_TOKEN: LazyLock<String> = LazyLock::new(|| std::env::var("SLACK_TOKEN").unwrap());

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

#[derive(Clone)]
struct AppState {
    key: Key,
    submitted_users: Arc<RwLock<HashSet<u64>>>,
    slack_token: SlackApiToken,
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
    };
    let app = Router::new()
        .route("/", get(root))
        .route("/err", get(err))
        .route("/submit", post(submit))
        .layer(axum::Extension(tera))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:5555").await.unwrap();
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
