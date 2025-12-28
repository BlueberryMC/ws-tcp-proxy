# ws-tcp-proxy

Bidirectional proxy that bridges raw TCP and WebSocket binary frames.

## Features

- `tcp-ws`: TCP inbound to WebSocket outbound (binary frames).
- `ws-tcp`: WebSocket inbound to TCP outbound (binary frames).
- Optional HAProxy v2 header injection for `ws-tcp` using `cf-connecting-ip`.

## Build

```
cargo build --release
```

## Usage

### TCP → WebSocket

```
cargo run -- tcp-ws 127.0.0.1:9000 ws://127.0.0.1:8080
```

### WebSocket → TCP

```
cargo run -- ws-tcp 127.0.0.1:8080 127.0.0.1:9000
```

### WebSocket → TCP with HAProxy v2

```
cargo run -- ws-tcp 127.0.0.1:8080 127.0.0.1:9000 --haproxy-v2
```

Notes:
- In `ws-tcp` mode, the proxy inspects the WebSocket handshake for the `cf-connecting-ip` header.
- When `--haproxy-v2` is enabled, the proxy sends a HAProxy v2 PROXY header to the TCP backend using the `cf-connecting-ip` value if present, otherwise the remote socket address.
- If the header IP is IPv6 and the backend connection is IPv4, the proxy maps IPv4 to an IPv6-mapped address to produce a valid v2 header.

## Modes

- `tcp-ws <listen_addr> <ws_url>`
- `ws-tcp <listen_addr> <tcp_target> [--haproxy-v2 | --haproxy]`

## Example topology

```
[TCP client] -> (tcp-ws) -> [WebSocket server]
[WebSocket client] -> (ws-tcp) -> [TCP server]
```

## Fun fact

This project was originally called `WebSocketMC`.
