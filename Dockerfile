from rust:1.56.1 as builder
workdir /usr/src/tweet-broadcast
copy . .
run cargo install --path .

from debian:bullseye-slim
run apt update && apt upgrade -y && apt install -y tini && rm -rf /var/lib/apt/lists/*
workdir /var/lib/tweet-broadcast
copy --from=builder \
  /usr/local/cargo/bin/tweet-broadcast \
  /usr/local/bin/tweet-broadcast
entrypoint ["/usr/bin/tini", "--"]
cmd ["tweet-broadcast"]
