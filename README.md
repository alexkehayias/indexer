# About

WIP personal indexer service.

## Running it

Initialize index:

```
cargo run -- --init
```

Search notes:

```
cargo run -- query --term "testing" --vector
```

Index or re-index:

```
cargo run -- index --all
```

Run the server:

```
cargo run -- serve --port 2222
```

Search notes using the server:

```
http://localhost:2222/notes/search?query=test&include_similarity=true
```

## Docker

Build the image:

```
docker build -t "indexer:latest" .
```

Run a container:

```
docker run -p 2222:2222 -d indexer:latest
```

## Running on Dokku

1. In the `dokku` server, create the app
2. Create a directory for deploy keys on `dokku` under `/var/lib/dokku/data/storage/indexer/.ssh`
3. Generate a new key pair on the `dokku` server `ssh-keygen -q -t rsa -b 2048 -f "/var/lib/dokku/data/storage/indexer/.ssh/notes_id_rsa" -N ""`
4. Store the public key in the GitHub repo (Settings -> Deploy Keys)
5. Generate GitHub known hosts `sudo bash -c "ssh-keyscan -t rsa github.com >> /var/lib/dokku/data/storage/indexer/.ssh/known_hosts"`
7. Create data directory to persist indices between deploys `mkdir /var/lib/dokku/data/storage/indexer/data && dokku storage:mount indexer /var/lib/dokku/data/storage/indexer:/root/data`
8. Generate a VAPID key pair, private key with `openssl ecparam -genkey -name prime256v1 -out private_key.pem`, and public key with `openssl ec -in private_key.pem -pubout -outform DER|tail -c 65|base64|tr '/+' '_-'|tr -d '\n'` (remove the trailing `=` in the public key and hard code it into `index.js`)
9. Add the following environment variables using `dokku config:set indexer {ENV_VAR}`:
- `INDEXER_NOTES_REPO_URL` to the GitHub repo with notes
- `INDEXER_NOTES_DEPLOY_KEY_PATH` to `/root/.ssh`
- `INDEXER_STORAGE_PATH` to allow indices to persist between deploys
- `INDEXER_VAPID_KEY_PATH` for push notifications
- `INDEXER_NOTE_SEARCH_API_URL` for the note search AI tool
- `INDEXER_SEARXNG_API_URL` for the web search AI tool
- `DOKKU_DOCKERFILE_START_CMD` to `serve --host 0.0.0.0 --port 2222`
10. On local, add remote `git remote add dokku dokku@<dokku-host>:indexer`
11. Push to build and start `git push dokku main`
