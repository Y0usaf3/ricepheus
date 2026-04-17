#[macro_use]
extern crate dotenv_codegen;

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
use std::sync::Arc;
use tera::{Context, Tera};
use tokio::sync::RwLock;

const HT_CLIENT_ID: &str = dotenv!("CLIENT_ID");
const HT_CLIENT_SECRET: &str = dotenv!("CLIENT_SECRET");
const HT_REDIRECT_URI: &str = dotenv!("REDIRECT_URI");
const SLACK_TOKEN: &str = dotenv!("SLACK_TOKEN");

#[derive(serde::Serialize, Debug)]
struct CodeExchange<'a> {
    client_id: &'a str,
    client_secret: &'a str,
    code: String,
    redirect_uri: &'a str,
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
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.key.clone()
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
        key: Key::generate(), // we dont need to know the key lmfao, as the cookie will be stored
        // for a brieve amount of time
        submitted_users: Arc::new(RwLock::new(submitted_users)),
    };
    let app = Router::new()
        .route("/", get(root))
        .route("/err", get(err))
        .route("/submit", post(submit))
        .layer(axum::Extension(tera))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:5555")
        .await
        .unwrap();
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

    let project_details: Vec<String> = selected_projects
        .iter()
        .map(|p| {
            let hours = p.total_seconds / 3600;
            let minutes = (p.total_seconds % 3600) / 60;
            format!(
                "*{}* ({}h{}m) [{}s]",
                p.name, hours, minutes, p.total_seconds
            )
        })
        .collect();

    let message_text = format!(
        "User :github: *{}* submitted WWWWW projects: {} :boykisser-dance:",
        user.github_username,
        project_details.join(", ")
    );

    let client = SlackClient::new(
        SlackClientHyperConnector::new()
            .ok()
            .ok_or(Redirect::to("/err"))?,
    );
    let token_value: SlackApiTokenValue = SLACK_TOKEN.into();
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

    let post_chat_req = SlackApiChatPostMessageRequest::new(
        "#riceathons-very-private-discussion-bc-idk-what-name-i-should-choose-pfff".into(),
        message,
    );

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
                client_id: HT_CLIENT_ID,
                client_secret: HT_CLIENT_SECRET,
                code,
                redirect_uri: HT_REDIRECT_URI,
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
            HT_CLIENT_ID, HT_REDIRECT_URI
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
