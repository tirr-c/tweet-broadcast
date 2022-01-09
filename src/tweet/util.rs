use super::model;

macro_rules! concat_param {
    ($param1:literal $(, $param:literal)*) => {
        concat!($param1 $(, ",", $param)*)
    };
}

async fn retrieve_single(
    client: &reqwest::Client,
    id: &str,
) -> Result<model::ResponseItem<model::Tweet>, crate::Error>
{
    const TWEET_ENDPOINT: &'static str = "https://api.twitter.com/2/tweets";
    let mut url = TWEET_ENDPOINT.parse::<reqwest::Url>().unwrap();
    url.path_segments_mut().unwrap().push(id);
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

    let resp = client
        .get(url)
        .send()
        .await?
        .error_for_status()?;

    let resp = resp.json::<model::TwitterResponse<model::Tweet>>().await?;
    match resp {
        model::TwitterResponse::Ok(resp) => Ok(resp),
        model::TwitterResponse::Error(e) => Err(e.into())
    }
}

async fn retrieve_many(
    client: &reqwest::Client,
    ids: &[&str],
) -> Result<model::ResponseItem<Vec<model::Tweet>>, crate::Error>
{
    use futures_util::TryFutureExt;
    use futures_util::TryStreamExt;

    const TWEET_ENDPOINT: &'static str = "https://api.twitter.com/2/tweets";
    let mut url = TWEET_ENDPOINT.parse::<reqwest::Url>().unwrap();
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

    let mut req_fut = futures_util::stream::FuturesOrdered::new();
    for ids in ids.chunks(100) { // 100 tweets at a time
        let id_param = ids.join(",");
        let mut url = url.clone();
        url.query_pairs_mut().append_pair("ids", &id_param).finish();

        req_fut.push(
            client
                .get(url)
                .send()
                .map_err(crate::Error::from)
                .and_then(|resp| async move {
                    let resp = resp.error_for_status()?;
                    let resp = resp.json::<model::TwitterResponse<Vec<model::Tweet>>>().await?;
                    match resp {
                        model::TwitterResponse::Ok(resp) => Ok(resp),
                        model::TwitterResponse::Error(e) => Err(crate::Error::from(e)),
                    }
                })
        );
    }

    let base = if let Some(base) = req_fut.try_next().await? {
        base
    } else {
        panic!("no tweet IDs given");
    };
    let resp = req_fut.try_fold(
        base,
        |base, tweets| async move {
            Ok(base.merge(tweets, |mut t1, t2| { t1.extend(t2); t1 }))
        },
    ).await?;
    Ok(resp)
}

fn needs_augment<Data, Meta>(
    tweet: model::ResponseItemRef<'_, &model::Tweet, Data, Meta>,
) -> Option<String>
{
    let real_tweet = if let Some(rt_id) = tweet.view_data().get_retweet_source() {
        tweet.includes().get_tweet(rt_id).unwrap()
    } else {
        tweet.view_data()
    };

    // check media first
    let media_keys = real_tweet.media_keys();
    let key_count = media_keys.len();
    let media = media_keys
        .iter()
        .filter_map(|k| tweet.includes().get_media(k))
        .collect::<Vec<_>>();

    if key_count == media.len() {
        None
    } else {
        Some(real_tweet.id().to_owned())
    }
}

pub async fn load_augment_data<Data, Meta>(
    client: &reqwest::Client,
    tweet: model::ResponseItemRef<'_, &model::Tweet, Data, Meta>,
) -> Result<Option<model::ResponseItem<model::Tweet>>, crate::Error>
{
    if let Some(referenced_tweet_id) = needs_augment(tweet) {
        // retrieve tweet again
        sentry::add_breadcrumb(sentry::Breadcrumb {
            category: Some(String::from("tweet")),
            message: Some(String::from("Media info missing, fetching tweet info")),
            level: sentry::Level::Info,
            data: [
                (String::from("id"), tweet.view_data().id().into()),
                (String::from("referenced_tweet_id"), serde_json::Value::String(referenced_tweet_id.clone())),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        });
        let tweet = retrieve_single(client, &referenced_tweet_id).await?;
        Ok(Some(tweet))
    } else {
        Ok(None)
    }
}

pub async fn load_batch_augment_data<Data, Meta>(
    client: &reqwest::Client,
    tweets: model::ResponseItemRef<'_, &[model::Tweet], Data, Meta>,
) -> Result<Option<model::ResponseItem<Vec<model::Tweet>>>, crate::Error>
{
    let mut tweets_view = tweets.map(|v| v.iter());
    let mut ids = Vec::new();
    loop {
        let tweet = tweets_view.ref_mut_map(|it| it.next());
        let tweet = if let &Some(tweet) = tweet.view_data() {
            tweets.map(|_| tweet)
        } else {
            break;
        };
        if let Some(id) = needs_augment(tweet) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Ok(None);
    }

    let resp = retrieve_many(client, &ids.iter().map(|s| &**s).collect::<Vec<_>>()).await?;
    Ok(Some(resp))
}
