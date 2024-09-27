# About

WIP personal indexer service.

## Running it

Index notes:

```
cargo run -- --path "/path/to/org/notes"
```

Search notes:

```
cargo run -- --query "testing"
```

Run the server:

```
cargo run -- --serve --port 1111
```

Search notes using the server:

```
http://localhost:1111/notes/search?query=test
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
6. Mount the volume `dokku storage:mount indexer /var/lib/dokku/data/storage/indexer:/root/.ssh`
7. Add environment variables for `INDEXER_NOTES_REPO_URL` and `INDEXER_NOTES_DEPLOY_KEY_PATH="/root/.ssh"`
8. On local, add remote `git remote add dokku dokku@<dokku-host>:indexer`
9. Push to build and start `git push dokku main`
