.DEFAULT_GOAL := help
.PHONY: help version build release dist install uninstall \
        serve open send get ls \
        test test-unit test-e2e smoke fmt fmt-check lint audit check ci \
        docker-build docker-run clean

# Everything below can be overridden on the command line:
#   make serve PORT=9000 DIR=~/drop TOKEN=abc
#   make send FILES="./docs ./notes.md" SERVER=http://192.168.0.10:7878 TOKEN=abc
BIN      := fireshare
VERSION  := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
PORT     ?= 7878
DIR      ?= ./shared
TOKEN    ?=
SERVER   ?= http://127.0.0.1:$(PORT)
FILES    ?=
OUT      ?= ./received
SIZE_MIB ?= 256
IMAGE    ?= $(BIN):$(VERSION)

RELEASE  := target/release/$(BIN)
TOKEN_ARG = $(if $(TOKEN),--token $(TOKEN))
REMOTE    = --server $(SERVER) $(TOKEN_ARG)

# ---------------------------------------------------------------- general

help: ## 📖 Lists the available commands, by section
	@echo "🔥 fireshare $(VERSION): commands"
	@awk 'BEGIN {FS = ":.*?## "} \
		/^# -+ / { sub(/^# -+ /, ""); printf "\n  \033[1m%s\033[0m\n", $$0; next } \
		/^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "  variables: PORT=$(PORT) DIR=$(DIR) SERVER=$(SERVER) OUT=$(OUT) TOKEN=… FILES=…"

version: ## 🏷️  Shows the crate, toolchain and machine versions
	@echo "fireshare $(VERSION)"
	@rustc --version
	@cargo --version
	@uname -sm

# ---------------------------------------------------------------- build

build: ## 🔨 Builds (debug)
	@echo "🔨 Building (debug)..."
	@cargo build

release: ## 🚀 Optimized build for this machine (LTO + strip) in target/release/
	@echo "🚀 Building release..."
	@cargo build --release
	@ls -lh $(RELEASE) | awk '{print "   " $$9 "  " $$5}'

dist: ## 📦 Universal macOS binary (arm64 + x86_64) in dist/: runs on any Mac
	@echo "📦 Building arm64 and x86_64..."
	@rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
	@cargo build --release --target aarch64-apple-darwin
	@cargo build --release --target x86_64-apple-darwin
	@mkdir -p dist
	@lipo -create -output dist/$(BIN) \
		target/aarch64-apple-darwin/release/$(BIN) \
		target/x86_64-apple-darwin/release/$(BIN)
	@lipo -info dist/$(BIN)
	@ls -lh dist/$(BIN) | awk '{print "   " $$9 "  " $$5}'

install: ## 📥 Installs fireshare on the PATH (~/.cargo/bin)
	@cargo install --path . --locked
	@echo "✅ $$(command -v $(BIN))"

uninstall: ## 📤 Removes fireshare from the PATH
	@cargo uninstall $(BIN)

# ---------------------------------------------------------------- run

serve: release ## 📡 Shares DIR on PORT (TOKEN=… pins the token; without it one is generated)
	@$(RELEASE) serve --dir $(DIR) --port $(PORT) $(TOKEN_ARG)

open: ## 🌐 Opens the local server page in the browser
	@open "http://127.0.0.1:$(PORT)/"

send: release ## 📤 Sends FILES (files and folders) to SERVER
	@test -n "$(FILES)" || (echo "usage: make send FILES=\"./folder ./file\" SERVER=http://ip:$(PORT) TOKEN=…" && exit 2)
	@$(RELEASE) send $(FILES) $(REMOTE)

get: release ## 📥 Downloads everything from SERVER into OUT
	@$(RELEASE) get --all -o $(OUT) $(REMOTE)

ls: release ## 📋 Lists the files on SERVER
	@$(RELEASE) ls $(REMOTE)

# ---------------------------------------------------------------- quality

test: ## 🧪 Whole suite: unit + end to end (real server on a local port)
	@echo "🧪 Running tests..."
	@cargo test

test-unit: ## 🧪 Unit tests only (path rules)
	@cargo test --lib

test-e2e: ## 🔌 End-to-end tests only (tests/transfer.rs)
	@cargo test --test transfer

smoke: release ## 💨 Release binary on a real port: send, ls, get, cmp, wrong token (SIZE_MIB=256)
	@SIZE_MIB=$(SIZE_MIB) scripts/smoke.sh $(RELEASE)

fmt: ## 🎨 Formats the code (cargo fmt)
	@echo "🎨 Formatting..."
	@cargo fmt

fmt-check: ## 🎨 Checks formatting without changing anything
	@cargo fmt --check

lint: ## 🔍 Strict clippy on all targets: any warning fails the build
	@echo "🔍 Running clippy..."
	@cargo clippy --all-targets -- -D warnings

audit: ## 🛡️  Known vulnerabilities in dependencies (cargo-audit, if installed)
	@command -v cargo-audit >/dev/null \
		&& cargo audit \
		|| echo "⚠️  cargo-audit not installed: cargo install cargo-audit --locked"

check: fmt-check lint test ## ✅ Runs everything CI runs, locally

ci: check ## 🤖 Alias of check, used by the CI pipeline

# ---------------------------------------------------------------- docker

docker-build: ## 🐳 Static Linux server image (musl, FROM scratch)
	@echo "🐳 Building $(IMAGE)..."
	@docker build -t $(IMAGE) -t $(BIN):latest .
	@docker image ls $(IMAGE) --format '   {{.Repository}}:{{.Tag}}  {{.Size}}'

docker-run: docker-build ## 🐳 Runs the server in a container (DIR mounted at /shared, PORT exposed)
	@mkdir -p $(DIR)
	@docker run --rm -it -p $(PORT):7878 -v "$$(cd $(DIR) && pwd)":/shared \
		$(if $(TOKEN),-e FIRESHARE_TOKEN=$(TOKEN)) $(IMAGE)

# ---------------------------------------------------------------- maintenance

clean: ## 🧹 Removes build artifacts (leaves DIR and OUT alone)
	@echo "🧹 Cleaning..."
	@cargo clean
	@rm -rf dist
