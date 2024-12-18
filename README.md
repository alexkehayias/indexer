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
6. Mount the volume `dokku storage:mount indexer /var/lib/dokku/data/storage/indexer:/root/.ssh`
7. Create data directory to persist indices between deploys `mkdir /var/lib/dokku/data/storage/indexer/data && dokku storage:mount indexer /var/lib/dokku/data/storage/indexer:/root/data`
7. Add environment variables for `INDEXER_NOTES_REPO_URL` and `INDEXER_NOTES_DEPLOY_KEY_PATH="/root/.ssh"` and `INDEXER_STORAGE_PATH` (this will allow indices to be persisted between deploys)
8. On local, add remote `git remote add dokku dokku@<dokku-host>:indexer`
9. Push to build and start `git push dokku main`
