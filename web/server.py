from __future__ import annotations

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit


WEB_ROOT = Path(__file__).resolve().parent


class WebsiteHandler(SimpleHTTPRequestHandler):
    def end_headers(self) -> None:
        if urlsplit(self.path).path == "/downloads/pmm_installer.exe":
            self.send_header(
                "Content-Disposition",
                'attachment; filename="pmm_installer.exe"',
            )
        super().end_headers()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Serve the App Store Store website and release downloads."
    )
    parser.add_argument(
        "--host",
        default="127.0.0.1",
        help="Interface to bind (default: localhost only).",
    )
    parser.add_argument(
        "--port",
        type=int,
        default=8000,
        help="Port to listen on (default: 8000).",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    handler = partial(WebsiteHandler, directory=str(WEB_ROOT))
    server = ThreadingHTTPServer((args.host, args.port), handler)
    print(f"Serving {WEB_ROOT} at http://{args.host}:{args.port}/")
    print("Press Ctrl+C to stop.")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopping web server.")
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
