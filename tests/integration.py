#!/usr/bin/env python3
"""End-to-end tests for localhost-rs (stdlib only).

Usage:
    cargo build --release
    python3 tests/integration.py            # spawns target/release/localhost
    python3 tests/integration.py --external # server already running on :8080/:8081
    python3 tests/integration.py --slow     # also run the slow timeout tests
"""
import http.client
import os
import socket
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOST, PORT, PORT2 = "127.0.0.1", 8080, 8081

passed, failed = 0, []


def check(name, cond, detail=""):
    global passed
    if cond:
        passed += 1
        print("  ok   %s" % name)
    else:
        failed.append(name)
        print("  FAIL %s %s" % (name, detail))


def request(method, path, body=None, headers=None, port=PORT):
    c = http.client.HTTPConnection(HOST, port, timeout=15)
    c.request(method, path, body=body, headers=headers or {})
    r = c.getresponse()
    data = r.read()
    hdrs = {k.lower(): v for k, v in r.getheaders()}
    c.close()
    return r.status, hdrs, data


def raw(payload, port=PORT, timeout=15, read_all=True):
    s = socket.create_connection((HOST, port), timeout=timeout)
    s.sendall(payload)
    chunks = []
    try:
        while True:
            d = s.recv(65536)
            if not d:
                break
            chunks.append(d)
            if not read_all:
                break
    except socket.timeout:
        pass
    s.close()
    return b"".join(chunks)


def status_of(resp):
    return int(resp.split(b" ", 2)[1]) if resp.startswith(b"HTTP/") else None


def multipart(filename, content, field="file"):
    b = "----testboundary123"
    body = (
        "--%s\r\nContent-Disposition: form-data; name=\"%s\"; filename=\"%s\"\r\n"
        "Content-Type: text/plain\r\n\r\n" % (b, field, filename)
    ).encode() + content + ("\r\n--%s--\r\n" % b).encode()
    return body, {"Content-Type": "multipart/form-data; boundary=%s" % b}


def run_tests(slow):
    print("static files & errors")
    s, h, d = request("GET", "/")
    check("GET / -> 200", s == 200 and b"Hello from localhost-rs" in d)
    check("content-type html", h.get("content-type", "").startswith("text/html"))
    check("Date + Server headers", "date" in h and "server" in h)
    s, h, d = request("HEAD", "/")
    check("HEAD / -> 200, no body", s == 200 and d == b"" and int(h["content-length"]) > 0)
    s, h, d = request("GET", "/does-not-exist")
    check("GET missing -> 404 custom page", s == 404 and b"custom error page" in d)
    s, h, d = request("POST", "/")
    check("POST on GET-only route -> 405 + Allow", s == 405 and "allow" in h)
    s, h, d = request("PUT", "/")
    check("PUT -> 405", s == 405)
    s, h, d = request("GET", "/../../etc/passwd")
    check("path traversal -> 403", s == 403)
    s, h, d = request("GET", "/%2e%2e/%2e%2e/etc/passwd")
    check("encoded traversal -> 403", s == 403)
    s, h, d = request("GET", "/old")
    check("redirect 301", s == 301 and h.get("location") == "/files/")
    s, h, d = request("GET", "/files")
    check("dir without slash -> 301", s == 301 and h.get("location") == "/files/")
    s, h, d = request("GET", "/files/")
    check("autoindex lists files", s == 200 and b"a.txt" in d and b"b.html" in d)
    s, h, d = request("GET", "/files/a.txt")
    check("text file served", s == 200 and d.startswith(b"plain text"))

    print("malformed input")
    check("garbage -> 400", status_of(raw(b"GARBAGE\r\n\r\n")) == 400)
    check("bad version -> 400", status_of(raw(b"GET / HTTP/9.9\r\nHost: x\r\n\r\n")) == 400)
    check("missing Host -> 400", status_of(raw(b"GET / HTTP/1.1\r\n\r\n")) == 400)
    check("oversized header -> 400",
          status_of(raw(b"GET / HTTP/1.1\r\nHost: x\r\nX: " + b"a" * 40000 + b"\r\n\r\n")) == 400)

    print("keep-alive & pipelining")
    s = socket.create_connection((HOST, PORT), timeout=10)
    req = b"GET /files/a.txt HTTP/1.1\r\nHost: localhost\r\n\r\n"
    s.sendall(req + req)
    s.settimeout(2)
    data = b""
    try:
        while data.count(b"HTTP/1.1 200 OK") < 2:
            chunk = s.recv(65536)
            if not chunk:
                break
            data += chunk
    except socket.timeout:
        pass
    check("two pipelined responses", data.count(b"HTTP/1.1 200 OK") == 2, data[:80])
    s.close()
    resp = raw(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
    check("Connection: close honoured", b"Connection: close" in resp)

    print("virtual hosts & ports")
    s, h, d = request("GET", "/", headers={"Host": "second.local"})
    check("Host: second.local -> second server", s == 200 and b"Second virtual host" in d)
    s, h, d = request("GET", "/", headers={"Host": "unknown.example"})
    check("unknown Host -> default server", s == 200 and b"Hello from localhost-rs" in d)
    s, h, d = request("GET", "/", port=PORT2)
    check("second port serves", s == 200 and b"Hello from localhost-rs" in d)

    print("upload / delete")
    body, hdr = multipart("it test.txt", b"hello upload")
    s, h, d = request("POST", "/upload", body, hdr)
    check("multipart upload -> 201", s == 201 and b"it_test.txt" in d, d)
    s, h, d = request("GET", "/upload/it_test.txt")
    check("uploaded file readable", s == 200 and d == b"hello upload")
    s, h, d = request("DELETE", "/upload/it_test.txt")
    check("DELETE -> 204", s == 204)
    s, h, d = request("GET", "/upload/it_test.txt")
    check("deleted file -> 404", s == 404)
    s, h, d = request("DELETE", "/upload/it_test.txt")
    check("DELETE missing -> 404", s == 404)
    s, h, d = request("POST", "/upload?name=raw.bin", b"rawdata")
    check("raw body upload -> 201", s == 201)
    request("DELETE", "/upload/raw.bin")
    big = b"x" * (2 * 1024 * 1024)
    try:
        s, h, d = request("POST", "/upload?name=big.bin", big)
    except (ConnectionError, http.client.HTTPException, OSError):
        s = 413  # server may reset while we are still sending; acceptable
    check("body > limit -> 413", s == 413)
    check("oversized Content-Length -> 413",
          status_of(raw(b"POST /upload HTTP/1.1\r\nHost: x\r\nContent-Length: 99999999\r\n\r\n")) == 413)
    s, h, d = request("POST", "/upload", b"", {"Content-Length": "0"})
    check("empty upload -> 400", s == 400)

    print("chunked requests")
    payload = (b"POST /cgi-bin/echo.py HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n"
               b"Connection: close\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")
    resp = raw(payload)
    check("chunked body reaches CGI decoded",
          status_of(resp) == 200 and resp.endswith(b"hello world") and b"length=11" in resp, resp)
    s = socket.create_connection((HOST, PORT), timeout=10)
    for part in (b"POST /cgi-bin/echo.py HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n"
                 b"Connection: close\r\n\r\n", b"3\r\nab", b"c\r\n", b"0\r\n", b"\r\n"):
        s.sendall(part)
        time.sleep(0.1)
    data = b""
    while True:
        d = s.recv(65536)
        if not d:
            break
        data += d
    s.close()
    check("chunked split across packets", data.endswith(b"abc"), data)
    check("bad chunk size -> 400",
          status_of(raw(b"POST /cgi-bin/echo.py HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n")) == 400)

    print("CGI")
    s, h, d = request("GET", "/cgi-bin/hello.py?name=world")
    check("CGI GET", s == 200 and b"QUERY_STRING=name=world" in d and b"REQUEST_METHOD=GET" in d, d)
    check("CGI runs in script dir", b"cwd=" in d and d.rstrip().endswith(b"cgi-bin"), d)
    check("PATH_INFO is full script path", b"PATH_INFO=/" in d and b"hello.py" in d)
    s, h, d = request("POST", "/cgi-bin/echo.py", b"a=1&b=2", {"Content-Type": "application/x-www-form-urlencoded"})
    check("CGI POST body via stdin", s == 200 and d.endswith(b"a=1&b=2") and b"length=7" in d, d)
    s, h, d = request("GET", "/cgi-bin/missing.py")
    check("missing CGI script -> 404", s == 404)
    s, h, d = request("GET", "/cgi-bin/broken.py")
    check("broken CGI output -> 500", s == 500)

    print("cookies & sessions")
    s, h, d = request("GET", "/__session?name=Alice")
    cookie = h.get("set-cookie", "")
    check("session cookie issued", s == 200 and cookie.startswith("SID="), cookie)
    sid = cookie.split(";")[0]
    s, h, d = request("GET", "/__session", headers={"Cookie": sid})
    check("session persists (visits=2, name kept)", b'"visits":2' in d and b'"name":"Alice"' in d, d)
    s, h, d = request("GET", "/__session", headers={"Cookie": "SID=bogus"})
    check("unknown SID gets a fresh session", b'"new":true' in d)
    s, h, d = request("GET", "/__session/logout", headers={"Cookie": sid})
    s, h, d = request("GET", "/__session", headers={"Cookie": sid})
    check("logout invalidates session", b'"new":true' in d)

    print("concurrency")
    errors = []

    def worker():
        try:
            for _ in range(20):
                s, _, _ = request("GET", "/files/a.txt")
                if s != 200:
                    errors.append(s)
        except Exception as e:  # noqa
            errors.append(repr(e))

    threads = [threading.Thread(target=worker) for _ in range(30)]
    [t.start() for t in threads]
    [t.join() for t in threads]
    check("30 clients x 20 requests", not errors, errors[:3])

    print("robustness")
    s = socket.create_connection((HOST, PORT), timeout=5)
    s.sendall(b"GET / HTT")
    s.close()  # abrupt close mid-request
    s = socket.create_connection((HOST, PORT), timeout=5)
    s.close()
    for _ in range(50):  # many idle connects
        socket.create_connection((HOST, PORT), timeout=5).close()
    check("still alive after abuse", request("GET", "/")[0] == 200)

    if slow:
        print("timeouts (slow)")
        t0 = time.time()
        s, h, d = request("GET", "/cgi-bin/slow.py")
        check("hung CGI -> 504 within ~10s", s == 504 and time.time() - t0 < 20, (s, time.time() - t0))
        s = socket.create_connection((HOST, PORT), timeout=60)
        s.sendall(b"GET / HTTP/1.1\r\nHost: x\r\nX-Slow: ")
        data = s.recv(4096)  # server should answer 408 after READ_TIMEOUT (15s)
        check("stalled request -> 408", status_of(data) == 408, data[:60])
        s.close()
        check("alive after timeouts", request("GET", "/")[0] == 200)


def main():
    slow = "--slow" in sys.argv
    proc = None
    if "--external" not in sys.argv:
        binary = os.path.join(ROOT, "target", "release", "localhost")
        if not os.path.exists(binary):
            binary = os.path.join(ROOT, "target", "debug", "localhost")
        if not os.path.exists(binary):
            sys.exit("build the project first: cargo build --release")
        proc = subprocess.Popen([binary, "config/server.conf"], cwd=ROOT)
        for _ in range(50):
            try:
                socket.create_connection((HOST, PORT), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)
    try:
        run_tests(slow)
    finally:
        if proc:
            proc.terminate()
            proc.wait(timeout=10)
    print("\n%d passed, %d failed" % (passed, len(failed)))
    for f in failed:
        print("  - " + f)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
