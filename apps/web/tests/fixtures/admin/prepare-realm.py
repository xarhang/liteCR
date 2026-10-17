#!/usr/bin/env python3
import json
import secrets
import shlex
import sys
from pathlib import Path

if len(sys.argv) != 8:
    raise SystemExit('usage: prepare-realm.py <run-dir> <base-realm.json> <translator-sub> <reviewer-sub> <publisher-sub> <administrator-sub> <registrar-sub>')

run_dir = Path(sys.argv[1])
base_realm = Path(sys.argv[2])
subjects = sys.argv[3:]
roles = ('translator', 'reviewer', 'publisher', 'administrator', 'registrar')
if len(set(subjects)) != len(roles):
    raise SystemExit('five unique OIDC subjects are required')

realm = json.loads(base_realm.read_text(encoding='utf-8'))
realm['users'] = []
users = {}
for role, subject in zip(roles, subjects, strict=True):
    username = f'{role}.e2e'
    password = secrets.token_urlsafe(32)
    realm['users'].append({
        'id': subject,
        'username': username,
        'firstName': 'Synthetic',
        'lastName': role.title(),
        'email': f'{username}@example.invalid',
        'emailVerified': True,
        'enabled': True,
        'realmRoles': [],
        'attributes': {},
    })
    users[role] = {'username': username, 'subject': subject, 'password': password}

client_secret = secrets.token_urlsafe(48)
realm['clients'][0]['secret'] = client_secret
realm['clients'][0]['redirectUris'] = ['http://127.0.0.1:4173/api/v1/access/oidc/callback']
password_file = run_dir / 'oidc-users.json'
password_file.write_text(json.dumps(users, ensure_ascii=False, indent=2), encoding='utf-8')
password_file.chmod(0o600)

bootstrap = {
    'KC_BOOTSTRAP_ADMIN_USERNAME': 'litecr-e2e-bootstrap',
    'KC_BOOTSTRAP_ADMIN_PASSWORD': secrets.token_urlsafe(36),
}
bootstrap_file = run_dir / 'keycloak-bootstrap.env'
bootstrap_file.write_text(''.join(f'{name}={shlex.quote(value)}\n' for name, value in bootstrap.items()), encoding='utf-8')
bootstrap_file.chmod(0o600)

import_file = run_dir / 'keycloak-install' / 'keycloak-26.8.0' / 'data' / 'import' / 'litecr-realm.json'
import_file.parent.mkdir(parents=True, exist_ok=True)
import_file.write_text(json.dumps(realm, ensure_ascii=False, indent=2), encoding='utf-8')
import_file.chmod(0o600)

runtime = {
    'LITECR_E2E_ADMIN_OIDC_CLIENT_SECRET': client_secret,
}
runtime_file = run_dir / 'runtime-secrets.env'
runtime_file.write_text(''.join(f'export {name}={shlex.quote(value)}\n' for name, value in runtime.items()), encoding='utf-8')
runtime_file.chmod(0o600)
