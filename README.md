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
