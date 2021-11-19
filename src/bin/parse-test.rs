use tweet_broadcast::tweet;

fn main() {
    let stdin = std::io::stdin();
    let data = serde_json::from_reader::<_, tweet::ResponseItem<tweet::Tweet>>(stdin.lock()).unwrap();
    dbg!(data);
}
