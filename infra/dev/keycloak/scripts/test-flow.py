#!/usr/bin/env python3
"""Exercise the disposable loopback OIDC browser flow without printing secrets."""

from __future__ import annotations

import argparse
import http.cookiejar
import json
import os
import pathlib
import sys
import urllib.error
import urllib.parse
import urllib.request
from html.parser import HTMLParser


class LoopbackCookiePolicy(http.cookiejar.DefaultCookiePolicy):
    """Treat Secure cookies as usable on this HTTP-only loopback fixture."""

    def return_ok_secure(self, cookie: http.cookiejar.Cookie, request: object) -> bool:
        host = urllib.parse.urlsplit(request.get_full_url()).hostname  # type: ignore[attr-defined]
        if host == "127.0.0.1":
            return True
        return super().return_ok_secure(cookie, request)


class LoginForms(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.forms: list[dict[str, object]] = []
        self._form: dict[str, object] | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "form":
            self._form = {"action": values.get("action", ""), "fields": {}}
        elif tag in {"input", "button"} and self._form is not None and values.get("name"):
            fields = self._form["fields"]
            assert isinstance(fields, dict)
            fields[values["name"]] = values.get("value", "")

    def handle_endtag(self, tag: str) -> None:
        if tag == "form" and self._form is not None:
            self.forms.append(self._form)
            self._form = None


def fail(step: str, detail: str) -> None:
    raise RuntimeError(f"{step}: {detail}")


def read_fixture_password(path: pathlib.Path) -> str:
    try:
        if os.name == "posix" and path.stat().st_mode & 0o077:
            fail("credentials", "fixture env file must not be accessible to group or other users")
        values = {}
        for line in path.read_text(encoding="utf-8").splitlines():
            if "=" in line:
                key, value = line.split("=", 1)
                values[key] = value
        password = values.get("LITECR_FIXTURE_USER_PASSWORD", "")
        if not password:
            fail("credentials", "synthetic user password is missing")
        return password
    except OSError:
        fail("credentials", "cannot read the WSL-home fixture env file")
    raise AssertionError("unreachable")


def request(
    opener: urllib.request.OpenerDirector,
    url: str,
    data: bytes | None = None,
    headers: dict[str, str] | None = None,
) -> object:
    try:
        req = urllib.request.Request(url, data=data, headers=headers or {})
        return opener.open(req, timeout=30)
    except urllib.error.HTTPError as error:
        return error
    except (urllib.error.URLError, TimeoutError):
        fail("http", "loopback request failed")
    raise AssertionError("unreachable")


def check_status(response: object, expected: set[int], step: str) -> object:
    status = response.code  # type: ignore[attr-defined]
    if status not in expected:
        error_code = ""
        try:
            payload = json.loads(response.read())  # type: ignore[attr-defined]
            code = payload.get("error", {}).get("code")
            if isinstance(code, str) and code.isascii() and code.replace("_", "").isalnum():
                error_code = f" ({code})"
        except (ValueError, OSError, AttributeError):
            pass
        path = urllib.parse.urlsplit(response.geturl()).path  # type: ignore[attr-defined]
        fail(step, f"unexpected HTTP status {status}{error_code} at {path}")
    return response


def write_storage_state(path: pathlib.Path, cookies: http.cookiejar.CookieJar) -> None:
    private_root = (pathlib.Path.home() / ".litecr-dev").resolve()
    target = path.resolve()
    if target != private_root and private_root not in target.parents:
        fail("storage state", "output must stay under the private .litecr-dev directory")
    private_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(private_root, 0o700)
    playwright_cookies = []
    for cookie in cookies:
        same_site = cookie._rest.get("SameSite", "Lax")
        same_site = str(same_site).capitalize()
        if same_site not in {"Strict", "Lax", "None"}:
            same_site = "Lax"
        playwright_cookies.append(
            {
                "name": cookie.name,
                "value": cookie.value,
                "domain": cookie.domain,
                "path": cookie.path,
                "expires": cookie.expires if cookie.expires is not None else -1,
                "httpOnly": bool(cookie._rest.get("HttpOnly")),
                "secure": cookie.secure,
                "sameSite": same_site,
            }
        )
    target.write_text(
        json.dumps({"cookies": playwright_cookies, "origins": []}), encoding="utf-8"
    )
    os.chmod(target, 0o600)


def run(
    origin: str,
    credentials: pathlib.Path,
    storage_state: pathlib.Path | None,
    preserve_session: bool,
) -> None:
    parsed = urllib.parse.urlsplit(origin)
    if (
        parsed.scheme != "http"
        or parsed.hostname != "127.0.0.1"
        or not parsed.port
        or parsed.path
        or parsed.query
        or parsed.fragment
        or parsed.username
        or parsed.password
    ):
        fail("configuration", "origin must be an http://127.0.0.1:<port> origin")

    password = read_fixture_password(credentials)
    cookies = http.cookiejar.CookieJar(policy=LoopbackCookiePolicy())
    opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies))
    check_status(request(opener, origin + "/api/v1/access/session"), {401}, "unauthenticated session")

    login_page = check_status(
        request(opener, origin + "/api/v1/access/oidc/login"), {200}, "OIDC login start"
    )
    if urllib.parse.urlsplit(login_page.geturl()).hostname != "127.0.0.1":  # type: ignore[attr-defined]
        fail("OIDC login", "provider redirect left loopback")

    forms = LoginForms()
    forms.feed(login_page.read().decode("utf-8", "replace"))  # type: ignore[attr-defined]
    form = next(
        (candidate for candidate in forms.forms if "username" in candidate["fields"]), None
    )
    if form is None:
        fail("OIDC login", "synthetic Keycloak login form was not returned")
    fields = form["fields"]
    assert isinstance(fields, dict)
    fields["username"] = "registrar.synthetic"
    fields["password"] = password
    action = urllib.parse.urljoin(login_page.geturl(), str(form["action"]))  # type: ignore[attr-defined]
    action_host = urllib.parse.urlsplit(action).hostname
    if action_host != "127.0.0.1":
        fail("OIDC login", "login form action left loopback")
    callback_result = check_status(
        request(
            opener,
            action,
            urllib.parse.urlencode(fields).encode("utf-8"),
            {"Content-Type": "application/x-www-form-urlencoded"},
        ),
        {200, 401, 404},
        "OIDC callback",
    )
    del callback_result

    session = check_status(
        request(opener, origin + "/api/v1/access/session"), {200}, "authenticated session"
    )
    try:
        if json.loads(session.read()).get("actor_kind") != "human":  # type: ignore[attr-defined]
            fail("authenticated session", "expected a human fixture actor")
    except (ValueError, OSError):
        fail("authenticated session", "invalid session response")

    csrf = next((cookie.value for cookie in cookies if cookie.name == "litecr_csrf"), None)
    if not csrf or len(csrf) < 32:
        fail("CSRF", "session CSRF cookie is missing or too short")
    if storage_state is not None:
        write_storage_state(storage_state, cookies)
    rejected = check_status(
        request(
            opener,
            origin + "/api/v1/access/logout",
            b"",
            {"Origin": origin, "X-CSRF-Token": "deliberately-invalid"},
        ),
        {401, 403},
        "invalid CSRF logout",
    )
    del rejected
    if preserve_session:
        if storage_state is None:
            fail("storage state", "--preserve-session requires --storage-state")
        print("PASS: isolated OIDC login/callback, session and CSRF rejection; session preserved for E2E.")
    else:
        check_status(
            request(
                opener,
                origin + "/api/v1/access/logout",
                b"",
                {"Origin": origin, "X-CSRF-Token": csrf},
            ),
            {204},
            "valid CSRF logout",
        )
        check_status(request(opener, origin + "/api/v1/access/session"), {401}, "post-logout session")
        print("PASS: isolated OIDC login/callback, session, CSRF rejection, logout and revocation.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--origin", default="http://127.0.0.1:18080")
    parser.add_argument(
        "--credentials",
        type=pathlib.Path,
        default=pathlib.Path.home() / ".litecr-dev" / "keycloak-fixture.env",
    )
    parser.add_argument(
        "--preserve-session",
        action="store_true",
        help="leave the isolated synthetic session active so browser E2E can use its state file",
    )
    parser.add_argument(
        "--storage-state",
        type=pathlib.Path,
        help="write authenticated browser cookies inside ~/.litecr-dev for isolated E2E only",
    )
    args = parser.parse_args()
    try:
        run(args.origin, args.credentials, args.storage_state, args.preserve_session)
    except RuntimeError as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
