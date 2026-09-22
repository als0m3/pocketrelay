#!/bin/sh
# Certbot deployment hook: reload nginx after each certificate renewal.
nginx -t -q && systemctl reload nginx
