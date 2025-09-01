use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::{stream, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json;
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
    url: String,
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

#[derive(Serialize)]
struct CloudflareRenderRequest {
    url: String,
}

#[derive(Deserialize)]
struct CloudflareRenderResponse {
    success: bool,
    result: Option<String>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct CloudflareMarkdownResponse {
    success: bool,
    result: Option<Vec<CloudflareMarkdownResult>>,
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct CloudflareMarkdownResult {
    data: String,
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

async fn fetch_content_with_cloudflare(
    cloudflare: &Cloudflare,
    url: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let headers = reqwest::header::HeaderMap::from_iter([
        (
            AUTHORIZATION,
            format!("Bearer {}", cloudflare.api_token).parse()?,
        ),
        (CONTENT_TYPE, "application/json".parse()?),
    ]);

    // Step 1: Browser rendering to get HTML
    let render_url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/browser-rendering/content",
        cloudflare.account_id
    );
    let render_request = CloudflareRenderRequest {
        url: url.to_string(),
    };

    println!("    > 正在通过 Cloudflare 浏览器渲染获取 HTML: {}", url);
    let render_response = client
        .post(&render_url)
        .headers(headers.clone())
        .json(&render_request)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await?;

    let render_data: CloudflareRenderResponse = render_response.json().await?;

    if !render_data.success {
        return Err(format!(
            "Cloudflare 浏览器渲染失败: {:?}",
            render_data.errors.unwrap_or_default()
        )
        .into());
    }

    let html_content = render_data.result.ok_or("No HTML content returned")?;

    // Step 2: Convert HTML to Markdown using Cloudflare AI
    let markdown_url = format!(
        "https://api.cloudflare.com/client/v4/accounts/{}/ai/tomarkdown",
        cloudflare.account_id
    );

    println!("    > 正在通过 Cloudflare AI 将 HTML 转换为 Markdown...");

    // Create multipart form data manually
    let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
    let mut body = String::new();

    body.push_str(&format!("--{}\r\n", boundary));
    body.push_str(
        "Content-Disposition: form-data; name=\"files\"; filename=\"virtual_file.html\"\r\n",
    );
    body.push_str("Content-Type: text/html\r\n\r\n");
    body.push_str(&html_content);
    body.push_str(&format!("\r\n--{}--\r\n", boundary));

    let markdown_response = client
        .post(&markdown_url)
        .header(AUTHORIZATION, format!("Bearer {}", cloudflare.api_token))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={}", boundary),
        )
        .body(body)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await?;

    let markdown_data: CloudflareMarkdownResponse = markdown_response.json().await?;

    if !markdown_data.success {
        return Err(format!(
            "Cloudflare AI Markdown 转换失败: {:?}",
            markdown_data.errors.unwrap_or_default()
        )
        .into());
    }

    let markdown_result = markdown_data.result.ok_or("No markdown result returned")?;
    if markdown_result.is_empty() {
        return Err("Cloudflare AI Markdown 转换未返回任何结果".into());
    }

    let markdown_content = &markdown_result[0].data;
    if markdown_content.is_empty() {
        return Err("Cloudflare AI Markdown 转换未返回任何内容".into());
    }

    Ok(markdown_content.trim().to_string())
}

async fn fetch_content_with_jina(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let jina_reader_url = format!("https://r.jina.ai/{}", url);
    let client = reqwest::Client::new();

    let headers = reqwest::header::HeaderMap::from_iter([
        ("Accept".parse()?, "text/plain".parse()?),
        ("User-Agent".parse()?, "MyBookmarkProcessor/1.0".parse()?),
    ]);

    println!("    > 正在通过 Jina Reader 获取内容: {}", url);
    let response = client
        .get(&jina_reader_url)
        .headers(headers)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await?;

    let full_text = response.text().await?;

    if let Some(content_part) = full_text.split("Markdown Content:\n").nth(1) {
        Ok(content_part.trim().to_string())
    } else {
        println!("    > 警告: Jina Reader 未返回预期的 'Markdown Content:' 格式");
        Ok(full_text.trim().to_string())
    }
}

async fn fetch_article_content(
    config: &Config,
    url: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    if url.contains("mp.weixin.qq.com") {
        if let Some(cloudflare) = &config.cloudflare {
            println!("  > 检测到微信公众号链接，将使用 Cloudflare 抓取...");
            return fetch_content_with_cloudflare(cloudflare, url).await;
        } else {
            println!("  > 检测到微信公众号链接但未配置 Cloudflare，使用 Jina Reader 抓取...");
            return fetch_content_with_jina(url).await;
        }
    } else {
        println!("  > 使用 Jina Reader 抓取...");
        return fetch_content_with_jina(url).await;
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

struct Cloudflare {
    account_id: String,
    api_token: String,
}

struct Config {
    miniflux: Miniflux,
    openai: OpenAi,
    cloudflare: Option<Cloudflare>,
    whitelist: HashSet<String>,
}

async fn generate_and_update_entry(
    config: &Config,
    entry: Entry,
    feed_site_url: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut content: String = entry.content.clone();

    // Check if the site is whitelisted
    if entry
        .feed
        .as_ref()
        .map_or(false, |feed| !config.whitelist.contains(&feed.site_url))
    {
        return Ok(());
    }

    // Check if content already has AI summary
    if content.starts_with("<pre") {
        return Ok(());
    }

    // Special handling for m.ichouti.cn - fetch content if empty or very short
    let is_ichouti = if let Some(site_url) = feed_site_url {
        site_url.contains("m.ichouti.cn")
    } else {
        entry
            .feed
            .as_ref()
            .map_or(false, |feed| feed.site_url.contains("m.ichouti.cn"))
    };

    if is_ichouti {
        println!("检测到 m.ichouti.cn feed 且内容为空或过短，尝试获取文章内容...");
        match fetch_article_content(config, &entry.url).await {
            Ok(fetched_content) => {
                if !fetched_content.trim().is_empty() {
                    content = fetched_content;
                    println!("成功获取到文章内容，长度: {}", content.len());
                } else {
                    println!("获取到的内容为空，跳过处理");
                    return Ok(());
                }
            }
            Err(e) => {
                println!("获取文章内容失败: {}", e);
                return Ok(());
            }
        }
    }

    // Skip if content is still empty after potential fetch
    if content.trim().is_empty() {
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

fn build_config(env: &Env) -> Config {
    let cloudflare = if let (Ok(account_id), Ok(api_token)) = (
        env.var("CLOUDFLARE_ACCOUNT_ID"),
        env.var("CLOUDFLARE_API_TOKEN"),
    ) {
        Some(Cloudflare {
            account_id: account_id.to_string(),
            api_token: api_token.to_string(),
        })
    } else {
        None
    };

    Config {
        whitelist: env
            .var("WHITELIST_URL")
            .unwrap()
            .to_string()
            .split(',')
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
        cloudflare,
    }
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let config = build_config(&env);

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
        .map(|entry| {
            let config = &config;
            async move { generate_and_update_entry(config, entry, None).await }
        })
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

    let config = build_config(&env);

    if !config.whitelist.contains(&webhook_payload.feed.site_url) {
        return Response::ok("Ignored non-whitelist feed");
    };

    // 处理每个新文章的生成和更新，限制并发为 5 个任务
    let max_concurrent_tasks = 5;

    let feed_site_url = &webhook_payload.feed.site_url;
    let _: Vec<_> = stream::iter(webhook_payload.entries)
        .map(|entry| {
            let config = &config;
            let site_url = feed_site_url;
            async move { generate_and_update_entry(config, entry, Some(site_url)).await }
        })
        .buffer_unordered(max_concurrent_tasks)
        .collect()
        .await;

    Response::ok("Webhook handled")
}
