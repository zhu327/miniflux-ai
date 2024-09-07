use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::{stream, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashSet;
use worker::{event, Context, Env, Method, Request, Response, ScheduleContext, ScheduledEvent};

#[derive(Debug, Deserialize)]
struct Feed {
    site_url: String,
}

#[derive(Debug, Deserialize)]
struct Entry {
    id: u64,
    content: String,
    feed: Option<Feed>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    entries: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct WebhookPayload {
    event_type: String,
    feed: Feed,
    entries: Vec<Entry>,
}

#[derive(Serialize)]
struct UpdateRequest {
    content: String,
}

async fn get_entries(
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<ApiResponse, Box<dyn std::error::Error>> {
    // 创建 HTTP 客户端
    let client = reqwest::Client::new();

    // 使用 Basic Auth 进行身份验证
    let auth = format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", username, password))
    );

    // 发送 GET 请求
    let response = client
        .get(&format!("{}/v1/entries?status=unread&limit=100", base_url))
        .header(AUTHORIZATION, auth)
        .header(CONTENT_TYPE, "application/json")
        .send()
        .await?
        .json::<ApiResponse>()
        .await?;

    Ok(response)
}

async fn update_entry(
    base_url: &str,
    username: &str,
    password: &str,
    id: u64,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();

    let auth = format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", username, password))
    );

    let url = format!("{}/v1/entries/{}", base_url, id);
    let update_request = UpdateRequest {
        content: content.to_string(),
    };

    client
        .put(&url)
        .header(AUTHORIZATION, auth)
        .header(CONTENT_TYPE, "application/json")
        .json(&update_request) // 将请求体序列化为 JSON
        .send()
        .await?
        .error_for_status()?;

    Ok(())
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    message: Message,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

async fn request_openai_chat_completion(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: Vec<Message>,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let request_body = ChatCompletionRequest {
        model: model.to_string(),
        messages,
    };

    let response = client
        .post(&format!("{}/v1/chat/completions", base_url))
        .header(AUTHORIZATION, format!("Bearer {}", api_key))
        .header(CONTENT_TYPE, "application/json")
        .json(&request_body)
        .send()
        .await?;

    if response.status().is_success() {
        let completion_response: ChatCompletionResponse = response.json().await?;
        Ok(completion_response.choices[0].message.content.clone())
    } else {
        let error_message = response.text().await?;
        Err(format!("Error: {:?}", error_message).into())
    }
}

struct Miniflux {
    url: String,
    username: String,
    password: String,
}

struct OpenAi {
    url: String,
    token: String,
    model: String,
}

struct Config {
    miniflux: Miniflux,
    openai: OpenAi,
    whitelist: HashSet<String>,
}

async fn generate_and_update_entry(
    config: &Config,
    entry: Entry,
) -> Result<(), Box<dyn std::error::Error>> {
    let content: &str = &entry.content;
    // Check if the content should be summarized and if the site is whitelisted
    if content.starts_with("<pre")
        || entry
            .feed
            .as_ref()
            .map_or(false, |feed| !config.whitelist.contains(&feed.site_url))
    {
        return Ok(());
    }

    let messages = vec![
        Message {
            role: "system".to_string(),
            content: "Please summarize the content of the article under 150 words in Chinese. Do not add any additional Character、markdown language to the result text. 请用不超过150个汉字概括文章内容。结果文本中不要添加任何额外的字符、Markdown语言。".to_string(),
        },
        Message {
            role: "user".to_string(),
            content: format!(
                "The following is the input content:\n---\n {}",
                content,
            ),
        },
    ];

    // Generate summary
    if let Ok(summary) = request_openai_chat_completion(
        &config.openai.url,
        &config.openai.token,
        &config.openai.model,
        messages,
    )
    .await
    {
        if !summary.trim().is_empty() {
            let updated_content = format!(
                "<pre style=\"white-space: pre-wrap;\"><code>\n💡AI 摘要：\n{}</code></pre><hr><br />{}",
                summary, content
            );

            // Update the entry
            update_entry(
                &config.miniflux.url,
                &config.miniflux.username,
                &config.miniflux.password,
                entry.id,
                &updated_content,
            )
            .await?;
        }
    }

    Ok(())
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let config = &Config {
        whitelist: env
            .var("WHITELIST_URL")
            .unwrap()
            .to_string()
            .split(",")
            .map(|s| s.to_string())
            .collect(),
        openai: OpenAi {
            url: env.var("OPENAI_URL").unwrap().to_string(),
            token: env.var("OPENAI_TOKEN").unwrap().to_string(),
            model: env.var("OPENAI_MODEL").unwrap().to_string(),
        },
        miniflux: Miniflux {
            url: env.var("MINIFLUX_URL").unwrap().to_string(),
            username: env.var("MINIFLUX_USERNAME").unwrap().to_string(),
            password: env.var("MINIFLUX_PASSWORD").unwrap().to_string(),
        },
    };

    // 查询未读文章
    let entries = get_entries(
        &config.miniflux.url,
        &config.miniflux.username,
        &config.miniflux.password,
    )
    .await
    .unwrap();

    // 生成摘要并更新的并发任务
    let max_concurrent_tasks = 5;

    // Create a stream to process tasks with concurrency limit
    let _: Vec<_> = stream::iter(entries.entries)
        .map(|entry| async move { generate_and_update_entry(config, entry).await })
        .buffer_unordered(max_concurrent_tasks)
        .collect()
        .await;
}

// 验证 Miniflux 的 Webhook 请求签名
fn validate_signature(secret: &str, payload: &str, signature: &str) -> bool {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC can take key of any size");
    mac.update(payload.as_bytes());
    let result = mac.finalize();
    let computed_signature = hex::encode(result.into_bytes());
    computed_signature == signature
}

#[event(fetch)]
async fn main(mut req: Request, env: Env, _: Context) -> worker::Result<Response> {
    // 检查请求方法
    if req.method() != Method::Post {
        return Response::error("Method Not Allowed", 405);
    }

    // 提取请求体和签名
    let payload = req.text().await?;
    let signature = req.headers().get("X-Miniflux-Signature")?.unwrap();

    let secret = env.var("MINIFLUX_WEBHOOK_SECRET").unwrap().to_string();

    // 验证签名
    if !validate_signature(&secret, &payload, &signature) {
        return Response::error("Invalid signature", 401);
    };

    // 解析请求体
    let webhook_payload: WebhookPayload = serde_json::from_str(&payload)?;

    if webhook_payload.event_type != "new_entries" {
        return Response::ok("Ignored non-new_entries event");
    };

    let config = &Config {
        whitelist: env
            .var("WHITELIST_URL")
            .unwrap()
            .to_string()
            .split(",")
            .map(|s| s.to_string())
            .collect(),
        openai: OpenAi {
            url: env.var("OPENAI_URL").unwrap().to_string(),
            token: env.var("OPENAI_TOKEN").unwrap().to_string(),
            model: env.var("OPENAI_MODEL").unwrap().to_string(),
        },
        miniflux: Miniflux {
            url: env.var("MINIFLUX_URL").unwrap().to_string(),
            username: env.var("MINIFLUX_USERNAME").unwrap().to_string(),
            password: env.var("MINIFLUX_PASSWORD").unwrap().to_string(),
        },
    };

    if !config.whitelist.contains(&webhook_payload.feed.site_url) {
        return Response::ok("Ignored non-whitelist feed");
    };

    // 处理每个新文章的生成和更新，限制并发为 5 个任务
    let max_concurrent_tasks = 5;

    let _: Vec<_> = stream::iter(webhook_payload.entries)
        .map(|entry| async move { generate_and_update_entry(config, entry).await })
        .buffer_unordered(max_concurrent_tasks)
        .collect()
        .await;

    Response::ok("Webhook handled")
}
