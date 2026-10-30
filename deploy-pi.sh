#!/usr/bin/env bash
# Update the existing home-core installation. Configuration and SQLite stay on Pi.
set -euo pipefail
cd "$(dirname "$0")"
PI_HOST=${PI_HOST:-home-core}
revision=$(git rev-parse HEAD)
release="$(git rev-parse --short=7 HEAD)-$(date -u +%Y%m%dT%H%M%SZ)-$$"
if [[ -n $(git status --porcelain --untracked-files=normal) ]]; then
  echo 'Deploying current local files, including any uncommitted source changes.'
fi

echo '==> Checking Pi access and existing installation...'
ssh -o BatchMode=yes "$PI_HOST" 'set -eu
command -v cc >/dev/null
. "$HOME/.cargo/env"
command -v cargo >/dev/null
sudo -n test -s /etc/hems-core/hems-core.env
sudo -n test -s /var/lib/hems-core/history.sqlite3
sudo -n test -L /opt/hems-core/current
systemctl is-active --quiet hems-core
'

echo '==> Building frontend locally...'
npm --prefix frontend run build

echo '==> Syncing source and compiled frontend assets...'
ssh "$PI_HOST" "mkdir -p hems-core/releases/$release/frontend"
rsync -az Cargo.toml Cargo.lock src "$PI_HOST:hems-core/releases/$release/"
rsync -az frontend/dist "$PI_HOST:hems-core/releases/$release/frontend/"

echo '==> Building on Pi (one job, low priority)...'
ssh "$PI_HOST" bash -s -- "$release" "$revision" <<'REMOTE'
set -euo pipefail
release=$1
revision=$2
base="$HOME/hems-core"
. "$HOME/.cargo/env"
# Refuse concurrent deployments rather than racing the active release.
exec 9>"$base/deploy.lock"
flock -n 9 || { echo 'Another deployment is running'; exit 1; }
# Reuse the first deployment's compiled dependencies, then keep one shared cache.
if [[ ! -e "$base/target" && -d "$base/releases/57a95df/target" ]]; then
  mv "$base/releases/57a95df/target" "$base/target"
fi
cd "$base/releases/$release"
CARGO_TARGET_DIR="$base/target" CARGO_BUILD_JOBS=1 nice -n 15 cargo build --release --locked
sudo -n install -d -m 755 "/opt/hems-core/releases/$release"
sudo -n install -m 755 "$base/target/release/hems-core" "/opt/hems-core/releases/$release/hems-core"
printf '%s\n' "$revision (local working files)" | sudo -n tee "/opt/hems-core/releases/$release/REVISION" >/dev/null
previous=$(readlink /opt/hems-core/current)
printf '%s\n' "$previous" > "$base/previous-release"
activate() {
  sudo -n ln -sfn "$1" /opt/hems-core/current.new
  sudo -n mv -Tf /opt/hems-core/current.new /opt/hems-core/current
  sudo -n systemctl restart hems-core
}
rollback() {
  echo '==> Verification failed; restoring previous binary...'
  activate "$previous"
  sudo -n journalctl -u hems-core -n 25 --no-pager
}
echo '==> Backing up live SQLite history before activation...'
sudo -n install -d -m 700 /var/backups/hems-core
sudo -n python3 - "$release" <<'PYBACKUP'
import os,sqlite3,sys
os.umask(0o077)
with sqlite3.connect('file:/var/lib/hems-core/history.sqlite3?mode=ro',uri=True) as source:
    with sqlite3.connect('/var/backups/hems-core/'+sys.argv[1]+'.sqlite3') as backup:
        source.backup(backup)
        assert backup.execute('PRAGMA integrity_check').fetchone()==('ok',)
PYBACKUP
echo '==> Restarting HEMS...' 
# The running binary is untouched until its replacement has built successfully.
if ! activate "/opt/hems-core/releases/$release"; then rollback; exit 1; fi
healthy=false
for attempt in $(seq 1 30); do
  if curl -fsS --max-time 5 http://127.0.0.1:3001/api/health >/dev/null; then healthy=true; break; fi
  sleep 2
done
if [[ $healthy != true ]]; then rollback; exit 1; fi
for path in / /api/health /api/version /api/energy /api/forecasts /api/history/status /api/modelling/summary; do
  echo "==> Checking $path"
  if ! curl -fsS --max-time 90 -o /dev/null "http://127.0.0.1:3001$path"; then rollback; exit 1; fi
done
if ! systemctl is-enabled --quiet hems-core; then rollback; exit 1; fi
curl -fsS http://127.0.0.1:3001/api/version
printf '\n==> Deployed %s. Previous binary: %s\n' "$release" "$previous"
REMOTE

echo '==> Done. Dashboard: https://hems.monstershome.net/'
echo '    Pi configuration, history, Pi-hole and Caddy were left in place.'
