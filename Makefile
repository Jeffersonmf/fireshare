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

# ---------------------------------------------------------------- geral

help: ## 📖 Lista os comandos disponíveis, por seção
	@echo "🔥 fireshare $(VERSION): comandos"
	@awk 'BEGIN {FS = ":.*?## "} \
		/^# -+ / { sub(/^# -+ /, ""); printf "\n  \033[1m%s\033[0m\n", $$0; next } \
		/^[a-zA-Z0-9_-]+:.*?## / { printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""
	@echo "  variáveis: PORT=$(PORT) DIR=$(DIR) SERVER=$(SERVER) OUT=$(OUT) TOKEN=… FILES=…"

version: ## 🏷️  Mostra versão do crate, do toolchain e da máquina
	@echo "fireshare $(VERSION)"
	@rustc --version
	@cargo --version
	@uname -sm

# ---------------------------------------------------------------- build

build: ## 🔨 Compila (debug)
	@echo "🔨 Compilando (debug)..."
	@cargo build

release: ## 🚀 Compila otimizado para esta máquina (LTO + strip) em target/release/
	@echo "🚀 Compilando release..."
	@cargo build --release
	@ls -lh $(RELEASE) | awk '{print "   " $$9 "  " $$5}'

dist: ## 📦 Binário universal macOS (arm64 + x86_64) em dist/: roda em qualquer Mac
	@echo "📦 Compilando arm64 e x86_64..."
	@rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
	@cargo build --release --target aarch64-apple-darwin
	@cargo build --release --target x86_64-apple-darwin
	@mkdir -p dist
	@lipo -create -output dist/$(BIN) \
		target/aarch64-apple-darwin/release/$(BIN) \
		target/x86_64-apple-darwin/release/$(BIN)
	@lipo -info dist/$(BIN)
	@ls -lh dist/$(BIN) | awk '{print "   " $$9 "  " $$5}'

install: ## 📥 Instala o fireshare no PATH (~/.cargo/bin)
	@cargo install --path . --locked
	@echo "✅ $$(command -v $(BIN))"

uninstall: ## 📤 Remove o fireshare do PATH
	@cargo uninstall $(BIN)

# ---------------------------------------------------------------- execução

serve: release ## 📡 Compartilha DIR na PORT (TOKEN=… fixa o token; sem ele, um é gerado)
	@$(RELEASE) serve --dir $(DIR) --port $(PORT) $(TOKEN_ARG)

open: ## 🌐 Abre a página do servidor local no navegador
	@open "http://127.0.0.1:$(PORT)/"

send: release ## 📤 Envia FILES (arquivos e pastas) para SERVER
	@test -n "$(FILES)" || (echo "uso: make send FILES=\"./pasta ./arquivo\" SERVER=http://ip:$(PORT) TOKEN=…" && exit 2)
	@$(RELEASE) send $(FILES) $(REMOTE)

get: release ## 📥 Baixa tudo de SERVER para OUT
	@$(RELEASE) get --all -o $(OUT) $(REMOTE)

ls: release ## 📋 Lista os arquivos de SERVER
	@$(RELEASE) ls $(REMOTE)

# ---------------------------------------------------------------- qualidade

test: ## 🧪 Toda a suíte: unitários + ponta a ponta (servidor real em porta local)
	@echo "🧪 Rodando testes..."
	@cargo test

test-unit: ## 🧪 Só os unitários (regras de caminho)
	@cargo test --lib

test-e2e: ## 🔌 Só os de ponta a ponta (tests/transfer.rs)
	@cargo test --test transfer

smoke: release ## 💨 Binário de release numa porta real: send, ls, get, cmp, token errado (SIZE_MIB=256)
	@SIZE_MIB=$(SIZE_MIB) scripts/smoke.sh $(RELEASE)

fmt: ## 🎨 Formata o código (cargo fmt)
	@echo "🎨 Formatando..."
	@cargo fmt

fmt-check: ## 🎨 Verifica formatação sem alterar nada
	@cargo fmt --check

lint: ## 🔍 Clippy estrito em todos os alvos: qualquer warning quebra o build
	@echo "🔍 Rodando clippy..."
	@cargo clippy --all-targets -- -D warnings

audit: ## 🛡️  Vulnerabilidades conhecidas nas dependências (cargo-audit, se instalado)
	@command -v cargo-audit >/dev/null \
		&& cargo audit \
		|| echo "⚠️  cargo-audit não instalado: cargo install cargo-audit --locked"

check: fmt-check lint test ## ✅ Roda tudo que o CI roda, localmente

ci: check ## 🤖 Alias de check, usado pelo pipeline de CI

# ---------------------------------------------------------------- docker

docker-build: ## 🐳 Imagem Linux estática (musl, FROM scratch) do servidor
	@echo "🐳 Buildando $(IMAGE)..."
	@docker build -t $(IMAGE) -t $(BIN):latest .
	@docker image ls $(IMAGE) --format '   {{.Repository}}:{{.Tag}}  {{.Size}}'

docker-run: docker-build ## 🐳 Roda o servidor em container (DIR montado em /shared, PORT exposta)
	@mkdir -p $(DIR)
	@docker run --rm -it -p $(PORT):7878 -v "$$(cd $(DIR) && pwd)":/shared \
		$(if $(TOKEN),-e FIRESHARE_TOKEN=$(TOKEN)) $(IMAGE)

# ---------------------------------------------------------------- manutenção

clean: ## 🧹 Remove artefatos de build (não toca em DIR nem em OUT)
	@echo "🧹 Limpando..."
	@cargo clean
	@rm -rf dist
