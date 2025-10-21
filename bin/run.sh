cd ./web-ui/ && npx tailwindcss -i ./src/input.css -o ./src/output.css -m && cd ../ && cargo test && cargo run -- serve --host localhost --port 2222
