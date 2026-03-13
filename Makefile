.PHONY: all ci build check test lint fmt clean dev dev-ui dev-daemon help

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*##' $(MAKEFILE_LIST) | awk -F ':.*## ' '{printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'

# --- Top-level ---

all: check test lint build ## Run check, test, lint, and build

ci: check test ui-test lint ui-build ## Run backend/frontend type-checks, lints, and tests

# --- Rust ---

build: ## Build Rust workspace
	cargo build

check: ## Type-check Rust workspace
	cargo check

test: ## Run Rust tests
	cargo test

clippy: ## Run clippy with warnings as errors
	cargo clippy --all-targets -- -D warnings

fmt: ## Format Rust code
	cargo fmt

fmt-check: ## Check Rust formatting
	cargo fmt -- --check

# --- UI ---

ui-install: ## Install UI dependencies
	cd ui && bun install

ui-build: ## Build UI (typecheck + vite)
	cd ui && bun run build

ui-test: ## Run UI tests
	cd ui && bun run test

ui-lint: ## Lint UI with Biome
	cd ui && bun run lint

ui-lint-fix: ## Auto-fix UI lint issues
	cd ui && bun run lint:fix

ui-fmt: ## Format UI code with Biome
	cd ui && bun run format

# --- Dev servers ---

dev-daemon: ## Run daemon in dev mode
	cargo run --bin ingotd

dev-ui: ## Run UI dev server
	cd ui && bun run dev

# --- Combined ---

dev: ## Run daemon and UI dev servers in parallel
	$(MAKE) dev-daemon & $(MAKE) dev-ui & wait

lint: clippy ui-lint fmt-check ## Run all linters (clippy + biome + fmt check)

# --- Clean ---

clean: ## Remove build artifacts
	cargo clean
	rm -rf ui/dist ui/node_modules/.tmp
