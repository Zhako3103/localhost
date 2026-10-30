#!/usr/bin/env python3
# Never finishes in time: used to test the CGI timeout (expects 504).
import time

time.sleep(60)
print("Content-Type: text/plain\r\n\r\ntoo late")
