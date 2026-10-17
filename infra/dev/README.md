# Local PostgreSQL foundation

Docker is optional. This Compose file is one convenient way to start a local development PostgreSQL 17.6 service; the liteCR server itself does not depend on Docker. For a Docker-free setup, install PostgreSQL locally and point `LITECR_DATABASE_URL` and `LITECR_TEST_DATABASE_URL` at a dedicated, disposable test database. Keep PostgreSQL bound to loopback, use synthetic data and a non-production credential, and do not point integration tests at a live or shared database.

The Compose service also binds its published port to loopback. Its password is synthetic and must never be used outside disposable development data.

## Docker-free development on Ubuntu/WSL

The example below uses Ubuntu 24.04, Rust 1.85.0 from `rust-toolchain.toml`, and PostgreSQL 18 from the PostgreSQL Apt Repository. On a fresh Ubuntu WSL installation, Ubuntu's default `postgresql` package may select a different major; follow the [official PostgreSQL Ubuntu instructions](https://www.postgresql.org/download/linux/ubuntu/) to configure PGDG, then install the explicit major below. It uses peer authentication over a Unix socket: no database password is stored in the repository or exposed on a port. These commands create a dedicated local test database; do not use it for patient or shared data.

Install the prerequisites once:

```bash
sudo apt update
sudo apt install -y postgresql-common ca-certificates
sudo /usr/share/postgresql-common/pgdg/apt.postgresql.org.sh
sudo apt update
sudo apt install -y postgresql-18 postgresql-contrib-18 build-essential curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.85.0 --component rustfmt --component clippy
source "$HOME/.cargo/env"
```

Check the cluster version before using it with liteCR:

```bash
pg_lsclusters
psql --version
```

Verify that the server reports the intended PostgreSQL major. Do not remove or upgrade an existing cluster without a separate dump/restore and reconciliation plan.

Create the database once. Replace `litecr_test` with another dedicated database name if needed. The role is the current WSL user, owns only this database and is not a superuser.

```bash
sudo service postgresql start
sudo -u postgres createuser --login "$USER"
sudo -u postgres createdb --owner="$USER" litecr_test
sudo -u postgres psql -v ON_ERROR_STOP=1 -d litecr_test -c 'CREATE EXTENSION pg_trgm;'
```

From the repository directory, run:

```bash
export LITECR_DATABASE_URL="postgres://$USER@localhost/litecr_test?host=/var/run/postgresql"
export LITECR_TEST_DATABASE_URL="$LITECR_DATABASE_URL"
# Development only: this disposable database owner also runs migrations.
export LITECR_MIGRATION_DATABASE_URL="$LITECR_DATABASE_URL"
cd apps/web
npm ci
npm run build
cd ../..
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run --locked -p litecr-server -- migrate
cargo run --locked -p litecr-server -- doctor
```

The Compose example pins its own PostgreSQL image. Development and CI checks do not establish a production support policy.

```powershell
docker compose -f infra/dev/compose.yaml up -d postgres
$env:LITECR_DATABASE_URL = 'postgres://litecr:litecr-local-only@127.0.0.1:54329/litecr'
$env:LITECR_MIGRATION_DATABASE_URL = $env:LITECR_DATABASE_URL
cargo run --locked -p litecr-server -- migrate
cargo run --locked -p litecr-server -- doctor
$env:LITECR_TEST_DATABASE_URL = $env:LITECR_DATABASE_URL
cargo test --locked -p litecr-infrastructure --test foundation
```

Stop the service with `docker compose -f infra/dev/compose.yaml down`. Keep the named volume if the local database is needed later. Removing it deletes the local test database.
