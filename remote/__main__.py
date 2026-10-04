import uvicorn

import sys

from .config import ENABLE_SESSIONS, HOST, PORT, TOKEN

if __name__ == "__main__":
    shown = "localhost" if HOST in ("127.0.0.1", "0.0.0.0") else HOST
    # Print the token only for interactive terminal sessions, never in pod logs.
    secret = f"#token={TOKEN}" if ENABLE_SESSIONS and sys.stdout.isatty() else ""
    print(f"\n  CustomRemote → http://{shown}:{PORT}/{secret}\n  API docs     → http://{shown}:{PORT}/docs\n")
    uvicorn.run("remote.server:app", host=HOST, port=PORT, log_level="warning",
                proxy_headers=True, forwarded_allow_ips="*")
