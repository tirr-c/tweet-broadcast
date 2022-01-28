use super::{model, TwitterClient};

macro_rules! concat_param {
    ($param1:literal $(, $param:literal)*) => {
        concat!($param1 $(, ",", $param)*)
    };
}

pub fn append_query_param_for_tweet(url: &mut reqwest::Url) {
    url.query_pairs_mut()
        .append_pair(
            "expansions",
            concat_param!["author_id", "attachments.media_keys"],
        )
        .append_pair(
            "tweet.fields",
            concat_param![
                "created_at",
                "entities",
                "public_metrics",
                "possibly_sensitive"
            ],
        )
        .append_pair(
            "user.fields",
            concat_param!["profile_image_url", "public_metrics"],
        )
        .append_pair(
            "media.fields",
            concat_param!["width", "height", "url", "preview_image_url"],
        )
        .finish();
}

fn needs_augment(tweet: &model::Tweet, includes: &model::ResponseIncludes) -> Option<String> {
    let real_tweet = if let Some(rt_id) = tweet.get_retweet_source() {
        includes.get_tweet(rt_id).unwrap()
    } else {
        tweet
    };

    let media_keys = real_tweet.media_keys();
    let is_complete = media_keys.iter().all(|k| includes.get_media(k).is_some());

    if is_complete {
        None
    } else {
        Some(real_tweet.id().to_owned())
    }
}

pub async fn load_batch_augment_data(
    client: &TwitterClient,
    tweets: &[model::Tweet],
    includes: &model::ResponseIncludes,
) -> Result<Option<model::ResponseItem<Vec<model::Tweet>>>, crate::Error> {
    let mut ids = Vec::new();
    for tweet in tweets {
        if let Some(id) = needs_augment(tweet, includes) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Ok(None);
    }

    // retrieve tweet again
    sentry::add_breadcrumb(sentry::Breadcrumb {
        category: Some(String::from("tweet")),
        message: Some(String::from("Media info missing, fetching tweet info")),
        level: sentry::Level::Info,
        data: [
            (
                String::from("ids"),
                tweets
                    .iter()
                    .map(|x| x.id().to_owned())
                    .collect::<Vec<_>>()
                    .into(),
            ),
            (String::from("referenced_tweet_ids"), ids.clone().into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let resp = client
        .retrieve(&ids.iter().map(|s| &**s).collect::<Vec<_>>())
        .await?;
    Ok(Some(resp))
}
