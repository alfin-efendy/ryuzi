# ryuzi monorepo — developer Makefile
# Bun workspaces (apps/*, packages/*) + Cargo workspace (crates/*, apps/cockpit/src-tauri).
# Quick start:  make setup  →  make dev
# List targets: make            (or `make help`)

SHELL := /bin/bash
.DEFAULT_GOAL := help

# Extra args for runnable targets, e.g.  make cli ARGS="status"
ARGS ?=

##@ Help
.PHONY: help
help: ## Show this help
	@awk 'BEGIN {FS = ":.*##"; printf "\nUsage: make \033[36m<target>\033[0m\n"} \
		/^[a-zA-Z0-9_.-]+:.*?##/ { printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2 } \
		/^##@/ { printf "\n\033[1m%s\033[0m\n", substr($$0, 5) }' $(MAKEFILE_LIST)

##@ Pre-development (setup)
.PHONY: setup install doctor
setup: install ## First-time setup (alias for install)
install: ## Install JS workspace deps + pre-fetch Rust crates
	bun install
	cargo fetch

doctor: ## Check the required toolchain is present
	@echo "bun:   $$(bun --version 2>/dev/null || echo 'MISSING — https://bun.sh')"
	@echo "cargo: $$(cargo --version 2>/dev/null || echo 'MISSING — https://rustup.rs')"
	@echo "rustc: $$(rustc --version 2>/dev/null || echo 'MISSING — https://rustup.rs')"
	@echo "tauri: $$(cd apps/cockpit && bun run tauri --version 2>/dev/null || echo 'run `make install`')"

##@ Development
.PHONY: dev cockpit cli
dev: ## Run the Cockpit desktop app with HMR (tauri dev)
	bun run cockpit:dev
cockpit: dev ## Alias for `dev`
cli: ## Run the ryuzi CLI (Rust) — pass flags via ARGS, e.g. make cli ARGS="status"
	cargo run -p ryuzi-cli -- $(ARGS)

##@ Build
.PHONY: build build-web run-release bundles
build: ## Build Cockpit release bundles (deb / rpm / AppImage)
	bun run cockpit:build
build-web: ## Build only the Cockpit frontend (tsc --noEmit + vite build)
	cd apps/cockpit && bun run build
run-release: ## Launch the compiled release binary (release chrome: no Reload/Inspect menu)
	./target/release/cockpit
bundles: ## List the installer bundles produced by `make build`
	@find target/release/bundle -maxdepth 2 -type f \
		\( -name '*.AppImage' -o -name '*.deb' -o -name '*.rpm' \) 2>/dev/null \
		|| echo "No bundles yet — run 'make build' first"

##@ Quality (tests / types / lint / format)
.PHONY: test test-rust test-all typecheck lint format fmt check
test: ## Run JS/TS unit tests (bun test)
	bun test
test-rust: ## Run Rust tests (cargo test, whole workspace)
	cargo test
test-all: test test-rust ## Run every test (JS + Rust)
typecheck: ## Type-check the workspace (tsc --noEmit)
	bun run typecheck
lint: ## Lint with Biome (CI mode — no writes)
	bun run lint
format: ## Auto-format JS/TS (Biome) + Rust (cargo fmt)
	bun run format
	cargo fmt
fmt: format ## Alias for `format`
check: typecheck lint test ## Pre-commit gate: types + lint + JS tests

##@ Cleanup
.PHONY: clean clean-all
clean: ## Remove build outputs (frontend dist + cargo target)
	rm -rf apps/*/dist
	cargo clean
clean-all: clean ## Also remove installed deps (node_modules)
	rm -rf node_modules apps/*/node_modules packages/*/node_modules
