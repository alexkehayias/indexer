#!/usr/bin/env zsh
set -euo pipefail

cd ./web-ui && npx tailwindcss -i ./src/input.css -o ./src/output.css -m && cd ..
# cargo test

# Start server in background
cargo run -- serve --host localhost --port 2222 &
PID=$!

# Function to cleanup server
cleanup() {
    kill $PID 2>/dev/null || true
    exit
}

# Trap exit signals to cleanup
trap cleanup EXIT INT TERM

# Wait for port 2222
HOST=localhost
PORT=2222
TIMEOUT=30
start=$(date +%s)
while ! nc -z ${HOST} ${PORT}; do
    (( $(date +%s) - start > TIMEOUT )) && { kill $PID 2>/dev/null || true; exit 1; }
    sleep 0.5
done

echo "Server ready"

# Reload the active Chrome browser tab
osascript ./bin/reloadChromeTab.scptd

wait $PID
