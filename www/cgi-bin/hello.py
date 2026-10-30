#!/usr/bin/env python3
import os

print("Content-Type: text/plain\r")
print("\r")
print("Hello from CGI")
for key in ("REQUEST_METHOD", "QUERY_STRING", "PATH_INFO", "SCRIPT_NAME", "SERVER_PORT"):
    print("%s=%s" % (key, os.environ.get(key, "")))
print("cwd=%s" % os.getcwd())
