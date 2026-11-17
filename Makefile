LOCK := flock $(HOME)/.cache/violet-build.lock

.PHONY: ui build lint test fmt run dev image

ui:  ## Build the UI that the server embeds
	cd web && npm ci && npm run build

build: ui
	$(LOCK) cargo build --release -p violet

fmt:
	cargo fmt --all
	cd web && npm run format

lint:
	cargo fmt --all --check
	$(LOCK) cargo clippy --workspace --all-targets -- -D warnings
	cd web && npm run check && npm run lint

test:
	$(LOCK) cargo test --workspace
	cd web && npm test

run:  ## Server on :8080 over ./library
	$(LOCK) cargo run -p violet -- serve --library ./library

dev:  ## Vite on :5173, proxying /api to `make run`
	cd web && npm run dev

image:
	docker build -f deploy/Dockerfile -t violet:dev .
