#!/usr/bin/env python3
import os
import sys

body = sys.stdin.buffer.read()
sys.stdout.write("Content-Type: text/plain\r\n\r\n")
sys.stdout.flush()
head = "method=%s length=%d\n" % (os.environ.get("REQUEST_METHOD"), len(body))
sys.stdout.buffer.write(head.encode() + body)
