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
2. Add environment variables for `INDEXER_NOTES_REPO_URL`
3. On local, add remote `git remote add dokku dokku@<dokku-host>:indexer`
4. Push to build and start `git push dokku main`
