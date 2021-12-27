use tweet_broadcast::tweet::model;

fn main() {
    let stdin = std::io::stdin();
    let data =
        serde_json::from_reader::<_, model::ResponseItem<model::Tweet>>(stdin.lock()).unwrap();
    dbg!(data);
}
