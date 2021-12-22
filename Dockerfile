from rust:1.57 as builder
workdir /usr/src/tweet-broadcast
copy . .
run cargo install --path .

from debian:bullseye-slim
workdir /var/lib/tweet-broadcast
copy --from=builder \
  /usr/local/cargo/bin/tweet-broadcast \
  /usr/local/bin/tweet-broadcast
entrypoint ["tweet-broadcast"]
