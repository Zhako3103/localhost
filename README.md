# localhost

A single-threaded, non-blocking **HTTP/1.1 server written from scratch in Rust**. It talks to the kernel directly through `libc` (raw `epoll`) and uses no async runtime and no HTTP crates.

## Highlights

- **One thread, one event loop.** Every listening socket, client socket and CGI pipe is registered in a single `epoll` instance.
- **Non-blocking I/O everywhere.** All file descriptors are switched to `O_NONBLOCK`; reads and writes never stall the loop and partial reads/writes are resumed on the next readiness event.
- **HTTP/1.1 protocol support.** Request-line and header parsing, keep-alive connections, `Content-Length` bodies and `Transfer-Encoding: chunked` bodies.
- **Virtual hosts.** The `Host` header selects the server configuration, so several sites can share one process and one port.
- **CGI.** Scripts are executed in a child process created with `fork` + `exec`; the child's stdin/stdout are connected through pipes that are driven by the same `epoll` loop.
- **Timeouts.** Slow or idle clients receive `408 Request Timeout`; CGI scripts that run for too long are terminated and the client receives `504 Gateway Timeout`.
- **Path hardening.** Request paths are normalised and checked against the configured document root, so `..` traversal and encoded escape sequences cannot leave the site directory.
- **Static files and error pages** with correct `Content-Type` and `Content-Length` headers.

## Architecture

```
            +--------------------------------------------+
            |                 epoll_wait                 |
            +----+------------+-------------+------------+
                 |            |             |
          listener fd     client fds     CGI pipe fds
                 |            |             |
              accept()    read/parse     read output
                          route/handle   write request body
                              |
                +-------------+--------------+
                |             |              |
          static file       CGI job     error response
                |             |              |
                +------> write buffer <------+
```

1. The listener sockets are bound and registered for `EPOLLIN`.
2. Each accepted connection becomes a state machine: *reading head* -> *reading body* -> *handling* -> *writing response* -> *keep-alive or close*.
3. Interest in `EPOLLOUT` is enabled only while a connection has pending output.
4. A timer sweep over active connections enforces the `408` / `504` deadlines.

## Build and run

Requirements: Linux, a stable Rust toolchain.

```bash
cargo build --release
cargo run --release
```

## Testing

Load test with [siege](https://github.com/JoeDog/siege):

```bash
siege -b -c 50 -t 30S http://127.0.0.1:8080/
```

Check for memory leaks and invalid accesses with Valgrind:

```bash
valgrind --leak-check=full --show-leak-kinds=all ./target/release/localhost
```

Manual protocol checks:

```bash
curl -v http://127.0.0.1:8080/
curl -v -H "Transfer-Encoding: chunked" -d "hello" http://127.0.0.1:8080/
curl -v --resolve site.local:8080:127.0.0.1 http://site.local:8080/
```

## What this project demonstrates

Systems programming in Rust, the Linux readiness-based I/O model, manual HTTP parsing, process management with `fork`/`exec`, and defensive handling of untrusted input.
