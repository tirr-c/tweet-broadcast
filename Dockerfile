from rust:1.65.0 as builder
workdir /usr/src/tweet-broadcast
copy . .
run cargo install --path ./crates/tweet-broadcast

from debian:bullseye-slim
workdir /var/lib/tweet-broadcast
copy --from=builder \
  /usr/local/cargo/bin/tweet-broadcast \
  /usr/local/bin/tweet-broadcast
entrypoint ["tweet-broadcast"]
