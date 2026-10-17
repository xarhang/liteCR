#!/usr/bin/env python3
import json
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

if len(sys.argv) != 3:
    raise SystemExit('usage: set-keycloak-passwords.py <run-dir> <loopback-keycloak-base-url>')

run_dir = Path(sys.argv[1])
base_url = sys.argv[2].rstrip('/')
if not base_url.startswith('http://127.0.0.1:'):
    raise SystemExit('refusing a non-loopback or non-HTTP fixture issuer')

bootstrap = {}
for line in (run_dir / 'keycloak-bootstrap.env').read_text(encoding='utf-8').splitlines():
    name, value = line.split('=', 1)
    # The generated username/password use URL-safe characters only.
    bootstrap[name] = value.strip("'")
users = json.loads((run_dir / 'oidc-users.json').read_text(encoding='utf-8'))

form = urllib.parse.urlencode({
    'grant_type': 'password',
    'client_id': 'admin-cli',
    'username': bootstrap['KC_BOOTSTRAP_ADMIN_USERNAME'],
    'password': bootstrap['KC_BOOTSTRAP_ADMIN_PASSWORD'],
}).encode()
request = urllib.request.Request(
    f'{base_url}/realms/master/protocol/openid-connect/token',
    data=form,
    headers={'Content-Type': 'application/x-www-form-urlencoded'},
)
try:
    with urllib.request.urlopen(request, timeout=10) as response:
        access_token = json.load(response)['access_token']
except (urllib.error.URLError, KeyError, TimeoutError):
    raise SystemExit('could not authenticate to the disposable Keycloak admin API')

for user in users.values():
    request = urllib.request.Request(
        f'{base_url}/admin/realms/litecr/users/{urllib.parse.quote(user["subject"], safe="")}/reset-password',
        data=json.dumps({'type': 'password', 'value': user['password'], 'temporary': False}).encode(),
        headers={'Authorization': f'Bearer {access_token}', 'Content-Type': 'application/json'},
        method='PUT',
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            if response.status != 204:
                raise SystemExit('Keycloak did not accept a synthetic test-user password')
    except (urllib.error.URLError, TimeoutError):
        raise SystemExit('Keycloak did not accept a synthetic test-user password')

print('configured five disposable OIDC user credentials')
