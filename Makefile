SHELL := /bin/sh

.DEFAULT_GOAL := help

.PHONY: help setup fmt fmt-check lint typecheck test check audit secret-scan

help: ## Show this help
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-12s %s\n", $$1, $$2}'

setup: ## Fetch locked dependencies and enable git hooks
	cargo fetch --locked
	chmod +x .githooks/pre-commit .githooks/pre-push
	git config core.hooksPath .githooks

fmt: ## Format the workspace
	cargo fmt --all

fmt-check: ## Fail if any file is not formatted
	cargo fmt --all -- --check

lint: ## Run clippy with warnings denied
	cargo clippy --all-targets -- -D warnings

typecheck: ## Typecheck all targets against the locked graph
	cargo check --all-targets --locked

test: ## Run the full test suite
	cargo test --locked --all-targets

check: fmt-check lint typecheck test ## Run format, lint, typecheck, tests

audit: ## Check lockfile for vulnerabilities
	cargo audit

secret-scan: ## Scan the git history for committed secrets
	gitleaks detect --source . --redact --no-banner --verbose
