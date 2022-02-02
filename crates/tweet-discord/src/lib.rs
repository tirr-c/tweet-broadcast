use tweet_model as model;

pub async fn send_webhook(
    client: &reqwest::Client,
    webhook_url: &reqwest::Url,
    tweet: &model::Tweet,
    includes: &model::ResponseIncludes,
) -> reqwest::Result<()> {
    let original_tweet = tweet;
    let original_author = includes
        .get_user(original_tweet.author_id().unwrap())
        .unwrap();

    let tweet_data = original_tweet
        .referenced_tweets()
        .iter()
        .find(|t| t.ref_type() == model::TweetReferenceType::Retweeted);
    let tweet_data = if let Some(ref_tweet) = tweet_data {
        includes.get_tweet(ref_tweet.id()).unwrap()
    } else {
        original_tweet
    };
    let author = includes.get_user(tweet_data.author_id().unwrap()).unwrap();

    let payload_media = if tweet_data.possibly_sensitive() {
        Vec::new()
    } else {
        tweet_data
            .media_keys()
            .iter()
            .map(|key| {
                let media = includes.get_media(key).unwrap();
                serde_json::json!({
                    "url": media.url_orig(),
                    "width": media.width(),
                    "height": media.height(),
                })
            })
            .collect::<Vec<_>>()
    };

    let mut payload_embed = vec![serde_json::json!({
        "author": {
            "name": format!("{} (@{})", author.name(), author.username()),
            "url": format!("https://twitter.com/{}", author.username()),
            "icon_url": author.profile_image_url_orig(),
        },
        "description": tweet_data.unescaped_text(),
        "timestamp": tweet_data.created_at(),
        "url": format!("https://twitter.com/{}/status/{}", author.username(), tweet_data.id()),
        "color": 1940464,
        "footer": {
            "text": "Twitter",
            "icon_url": "https://abs.twimg.com/favicons/favicon.png",
        },
        "image": payload_media.first(),
    })];
    payload_embed.extend(
        payload_media
            .into_iter()
            .map(|v| serde_json::json!({ "image": v }))
            .skip(1),
    );

    let content = format!(
        "{}https://twitter.com/{}/status/{}",
        if tweet_data.possibly_sensitive() { "\u{26a0} Possibly sensitive\n" } else { "" },
        author.username(),
        tweet_data.id(),
    );
    let payload = serde_json::json!({
        "username": format!("{} (@{})", original_author.name(), original_author.username()),
        "avatar_url": original_author.profile_image_url_orig(),
        "content": content,
        "embeds": payload_embed,
    });

    execute_webhook(client, webhook_url, &payload).await
}

pub async fn execute_webhook(
    client: &reqwest::Client,
    url: &reqwest::Url,
    payload: &serde_json::Value,
) -> reqwest::Result<()> {
    loop {
        log::trace!(
            "Sending payload {}",
            serde_json::to_string(payload).unwrap()
        );
        let resp = client
            .post(url.clone())
            .query(&[("wait", "true")])
            .json(payload)
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let duration = if let Some(reset_after) = resp.headers().get("x-ratelimit-reset-after")
            {
                let reset_after = reset_after.to_str().unwrap().parse::<f32>().unwrap();
                std::time::Duration::from_secs_f32(reset_after)
            } else if let Some(retry_after) = resp.headers().get(reqwest::header::RETRY_AFTER) {
                let retry_after = retry_after.to_str().unwrap().parse::<u64>().unwrap();
                std::time::Duration::from_secs(retry_after)
            } else {
                std::time::Duration::from_secs(5)
            };
            log::debug!("Webhook is ratelimited, retrying after {:?}", duration);
            tokio::time::sleep(duration).await;
        } else {
            resp.error_for_status()?;
            return Ok(());
        }
    }
}
