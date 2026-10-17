#!/usr/bin/env bash
set -euo pipefail
set +x

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
sql_file="$script_dir/grant-worker-database-access.sql"
[[ -f "$sql_file" && ! -L "$sql_file" ]] || { echo 'WORKER_ROLE_ACL_SQL_MISSING' >&2; exit 2; }
[[ -n "${LITECR_MIGRATION_DATABASE_URL:-}" ]] || { echo 'LITECR_MIGRATION_DATABASE_URL_REQUIRED' >&2; exit 2; }
command -v psql >/dev/null 2>&1 || { echo 'PSQL_REQUIRED' >&2; exit 2; }

# Keep the protected connection URL and password out of argv and repository files.
python3 - "$sql_file" <<'PY'
import os
import pathlib
import subprocess
import sys
import tempfile
from urllib.parse import parse_qsl, unquote, urlsplit

sql_file = pathlib.Path(sys.argv[1])
raw = os.environ["LITECR_MIGRATION_DATABASE_URL"]
parsed = urlsplit(raw)
if parsed.scheme not in ("postgres", "postgresql") or not parsed.path.strip("/"):
    raise SystemExit("MIGRATION_DATABASE_URL_INVALID")

query = dict(parse_qsl(parsed.query, keep_blank_values=True))
query_password = query.pop("password", "")
password = unquote(parsed.password or query_password)
settings = {
    "host": query.pop("host", parsed.hostname or ""),
    "port": query.pop("port", str(parsed.port or 5432)),
    "dbname": unquote(parsed.path.lstrip("/")),
    "user": unquote(parsed.username or ""),
}
allowed = {"sslmode", "sslcert", "sslkey", "sslrootcert", "application_name", "connect_timeout", "target_session_attrs", "options"}
if set(query) - allowed:
    raise SystemExit("MIGRATION_DATABASE_URL_UNSUPPORTED_OPTION")
settings.update(query)
if not settings["user"] or not settings["dbname"]:
    raise SystemExit("MIGRATION_DATABASE_URL_INVALID")
if any(char in str(value) for value in settings.values() for char in "\r\n#"):
    raise SystemExit("MIGRATION_DATABASE_URL_UNSUPPORTED_VALUE")

with tempfile.TemporaryDirectory(prefix="litecr-worker-pgservice-") as temp:
    os.chmod(temp, 0o700)
    service_file = pathlib.Path(temp) / "pg_service.conf"
    service_file.write_text("[litecr-worker-role-acl]\n" + "".join(f"{key}={value}\n" for key, value in settings.items()), encoding="utf-8")
    os.chmod(service_file, 0o600)
    env = os.environ.copy()
    for key in ("LITECR_MIGRATION_DATABASE_URL", "PGPASSWORD", "PGPASSFILE"):
        env.pop(key, None)
    env["PGSERVICEFILE"] = str(service_file)
    env["PGSERVICE"] = "litecr-worker-role-acl"
    if password:
        pass_file = pathlib.Path(temp) / "pgpass"
        escape = lambda value: value.replace("\\", "\\\\").replace(":", "\\:")
        credentials = (settings["host"], settings["port"], settings["dbname"], settings["user"], password)
        pass_file.write_text(":".join(map(escape, credentials)) + "\n", encoding="utf-8")
        os.chmod(pass_file, 0o600)
        env["PGPASSFILE"] = str(pass_file)
    subprocess.run(["psql", "-X", "-v", "ON_ERROR_STOP=1", "--no-psqlrc", "--file", str(sql_file)], env=env, check=True)
PY
