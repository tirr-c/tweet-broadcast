use chrono::{DateTime, Utc};

use super::tweet::{TweetPublicMetrics, UserPublicMetrics};

pub fn compute_score(
    tweet_metrics: &TweetPublicMetrics,
    user_metrics: &UserPublicMetrics,
    created_at: DateTime<Utc>,
) -> f64 {
    let time_diff = Utc::now() - created_at;
    let days_diff = time_diff.num_milliseconds() as f64 / (1000 * 60 * 60 * 24) as f64;

    let &TweetPublicMetrics {
        retweet_count: retweets,
        quote_count: quotes,
        like_count: likes,
        ..
    } = tweet_metrics;
    let &UserPublicMetrics {
        followers_count: followers,
        following_count: following,
        ..
    } = user_metrics;
    let rts = retweets + quotes;

    let rtparam = rts as f64 / 500.0;
    let rt_score = rtparam.log2().max(0.0) + rtparam.powi(2).min(1.0);

    let likeparam = likes as f64 / 2000.0;
    let like_score = likeparam.log2().max(0.0) + likeparam.min(1.0);

    let f_log_y = -2.0f64.log10() + 0.2f64.log10() * 1e-5 * followers as f64;
    let follower_adjust = 1.5f64 - 10.0f64.powf(f_log_y);
    let follow_rate_adjust =
        (1.0f64 - (4.0 / 9.0) * (followers as f64 / following as f64).powi(2)).max(0.0);

    ((rt_score + like_score) / follower_adjust - follow_rate_adjust).max(0.0)
        * 30.0
        * 1.5f64.powf((10.0 - days_diff) / 10.0)
}
