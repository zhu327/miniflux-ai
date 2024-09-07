# Miniflux AI Summarizer

This Cloudflare Workers tool automatically adds AI-generated summaries to articles in your Miniflux RSS reader. The summaries are generated using the OpenAI API and appended to articles in a user-friendly format.

## Features

- **Automated Summarization**: Automatically processes new articles received via Miniflux webhooks, generates concise summaries using AI, and updates the articles with the summaries.
- **Customizable**: Configure the list of whitelisted websites, API endpoints, and AI model parameters through environment variables.
- **Concurrency**: Uses asynchronous Rust features to handle multiple articles concurrently, ensuring quick processing.
- **Cloudflare Integration**: Deployed as a serverless function on Cloudflare Workers, leveraging the scalability and performance of Cloudflare's global network.
- **Recommended Model**: Uses the Cloudflare Workers AI model `@cf/qwen/qwen1.5-14b-chat-awq` for generating high-quality, concise summaries.

## Getting Started

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) installed
- A Miniflux instance with API access
- An OpenAI account with access to the model endpoint
- A Cloudflare account

### Installation

1. Clone the repository:
   ```bash
   git clone https://github.com/zhu327/miniflux-ai.git
   cd miniflux-ai
   ```

2. Deploy to Cloudflare Workers:
   ```bash
   npx wrangler deploy
   ```

### Configuration

The tool is configured using environment variables, which are set in the `wrangler.toml` file:

- `MINIFLUX_URL`: Your Miniflux instance URL.
- `MINIFLUX_USERNAME`: Your Miniflux username.
- `MINIFLUX_PASSWORD`: Your Miniflux password.
- `MINIFLUX_WEBHOOK_SECRET`: The secret key for validating incoming webhook requests from Miniflux.
- `OPENAI_URL`: The endpoint for the OpenAI API.
- `OPENAI_TOKEN`: Your OpenAI API token.
- `OPENAI_MODEL`: The model ID to use for generating summaries. We recommend using the `@cf/qwen/qwen1.5-14b-chat-awq` model for best results.
- `WHITELIST_URL`: A comma-separated list of website URLs that should be summarized.

### Usage

The tool is triggered by incoming webhook requests from Miniflux whenever new articles are available. If an article is from a whitelisted site and does not contain code blocks, it generates a summary and updates the article.

### Contributing

Contributions are welcome! Please feel free to submit issues, feature requests, or pull requests.

### License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
