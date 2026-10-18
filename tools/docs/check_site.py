"""Exercise the built documentation through HTTP under a GitHub project subpath."""

from __future__ import annotations

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import threading
from urllib.parse import quote
from urllib.request import urlopen


class ProjectHandler(SimpleHTTPRequestHandler):
    def do_GET(self):
        if not self.path.startswith('/liteCR/'):
            self.send_error(404)
            return
        self.path = self.path[len('/liteCR'):]
        super().do_GET()

    def log_message(self, *_):
        pass


def check(directory: Path) -> None:
    server = ThreadingHTTPServer(('127.0.0.1', 0), partial(ProjectHandler, directory=str(directory)))
    worker = threading.Thread(target=server.serve_forever, daemon=True)
    worker.start()
    try:
        files = sorted(p for p in directory.rglob('*') if p.is_file())
        for path in files:
            address = f'http://127.0.0.1:{server.server_port}/liteCR/' + quote(path.relative_to(directory).as_posix())
            with urlopen(address, timeout=5) as response:
                if response.status != 200 or response.read() != path.read_bytes():
                    raise ValueError(f'HTTP content mismatch: {path.name}')
        for name in ('index.html', 'docs/README.html', 'docs/BETA-SETUP.html', 'docs/USAGE.html', 'docs/API.html'):
            document = (directory / name).read_text()
            if '<main id="content"' not in document or '<script' in document or '<form' in document:
                raise ValueError(f'Missing content landmark or unexpected executable/input surface: {name}')
        if any(p.name == '.env' or p.suffix in {'.log', '.gz', '.pem'} for p in files):
            raise ValueError('Unexpected runtime or secret-bearing file in site output')
        print(f'HTTP project-path check passed: {len(files)} files served under /liteCR/')
    finally:
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', required=True, type=Path)
    check(parser.parse_args().directory.resolve())
