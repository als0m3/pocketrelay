import uvicorn

from .config import HOST, PORT, TOKEN

if __name__ == "__main__":
    shown = "localhost" if HOST in ("127.0.0.1", "0.0.0.0") else HOST
    print(f"\n  CustomRemote → http://{shown}:{PORT}/#token={TOKEN}\n  API docs     → http://{shown}:{PORT}/docs\n")
    uvicorn.run("remote.server:app", host=HOST, port=PORT, log_level="warning",
                proxy_headers=True, forwarded_allow_ips="*")
