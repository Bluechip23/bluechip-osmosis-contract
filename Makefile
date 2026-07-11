# ─── Config ───────────────────────────────────────────────────────────────────

WASM_TARGET       := wasm32-unknown-unknown
ARTIFACTS         := artifacts
# Env file used by the deploy/test targets. Override per-invocation:
#   make deploy ENV=osmosis_mainnet.env
ENV               := osmo_testnet.env

help:
	@echo "Build:"
	@echo "  make build              fast local wasm build (testnet only, not reproducible)"
	@echo "  make optimize-all       reproducible cosmwasm/optimizer builds (deployable)"
	@echo "  make check              cosmwasm-check every artifact"
	@echo "  make test               cargo test"
	@echo ""
	@echo "Osmosis testnet (osmo-test-5):"
	@echo "  make deploy             ./deploy_osmosis.sh \$$(ENV)  [ENV=osmo_testnet.env]"
	@echo "  make status             stack health overview (config, pricing probe, registry)"
	@echo "  make pool NAME='My Tok' SYMBOL=MYTOK   create a commit pool"
	@echo "  make lifecycle-test     full automated lifecycle (create->cross->swap->LP->route)"
	@echo ""
	@echo "Per-script usage lives in the headers of deploy_osmosis.sh and scripts/*.sh"

# ─── Build (local, non-optimized cargo build — local testing only) ──────────
# Canonical artifact names (factory.wasm, creator_pool.wasm, ...) are the
# PRODUCTION default-feature builds: real 48h timelocks. Deployable,
# reproducible, gas-optimized artifacts come from `make optimize-*`.
build:
	@mkdir -p $(ARTIFACTS)
	RUSTFLAGS="-C link-arg=-s" cargo build --release --target $(WASM_TARGET)
	cp target/$(WASM_TARGET)/release/creator_pool.wasm $(ARTIFACTS)/creator_pool.wasm
	cp target/$(WASM_TARGET)/release/factory.wasm $(ARTIFACTS)/factory.wasm
	cp target/$(WASM_TARGET)/release/router.wasm $(ARTIFACTS)/router.wasm

test:
	cargo test

# ─── Docker Optimizer (workspace) ────────────────────────────────────────────
optimize:
	docker run --rm -v "$$(pwd)":/code \
	  --mount type=volume,source="$$(basename "$$(pwd)")_cache",target=/code/target \
	  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
	  cosmwasm/workspace-optimizer:0.15.0

# ─── Docker Optimizer (per-contract) ─────────────────────────────────────────
optimize-creator-pool:
	docker run --rm -v ${CURDIR}:/code \
	  --mount type=volume,source=creator_pool_cache,target=/target \
	  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
	  cosmwasm/optimizer:0.16.0 ./creator-pool

optimize-factory:
	docker run --rm -v ${CURDIR}:/code \
	  --mount type=volume,source=factory_cache,target=/target \
	  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
	  cosmwasm/optimizer:0.16.0 ./factory
	@# factory declares TWO optimizer build variants:
	@#   name="prod"        → factory-prod.wasm        (NO features: real 48h timing) ← DEPLOY THIS
	@#   name="integration" → factory-integration.wasm (integration_short_timing; shell tests; NEVER ship)
	@# The canonical artifacts/factory.wasm that deploy tooling loads is the
	@# PROD build. Hard-fail if the prod artifact is missing rather than
	@# leave a stale factory.wasm.
	@if [ -f $(ARTIFACTS)/factory-prod.wasm ]; then \
	  cp $(ARTIFACTS)/factory-prod.wasm $(ARTIFACTS)/factory.wasm; \
	else \
	  echo "ERROR: factory-prod.wasm not produced by optimizer; refusing to leave a stale factory.wasm" >&2; \
	  exit 1; \
	fi

optimize-router:
	docker run --rm -v ${CURDIR}:/code \
	  --mount type=volume,source=router_cache,target=/target \
	  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
	  cosmwasm/optimizer:0.16.0 ./router

optimize-all: optimize-creator-pool optimize-factory optimize-router

# ─── Cosmwasm Check ──────────────────────────────────────────────────────────
check:
	cosmwasm-check $(ARTIFACTS)/creator_pool.wasm
	cosmwasm-check $(ARTIFACTS)/factory.wasm
	cosmwasm-check $(ARTIFACTS)/router.wasm

check-pool:
	cosmwasm-check $(ARTIFACTS)/creator_pool.wasm

check-factory:
	cosmwasm-check $(ARTIFACTS)/factory.wasm

# ─── Chain Deploy + Testnet Scripts ──────────────────────────────────────────
# Stores the wasms, instantiates factory + router, verifies via a config
# readback and a live x/twap pricing probe. Resumable; see the script header.
deploy:
	./deploy_osmosis.sh $(ENV)

status:
	ENV_FILE=$(ENV) scripts/status.sh $(POOL)

# make pool NAME='Alpha Creator' SYMBOL=ALPHA
pool:
	@test -n "$(NAME)" -a -n "$(SYMBOL)" \
	  || { echo "usage: make pool NAME='Alpha Creator' SYMBOL=ALPHA" >&2; exit 1; }
	ENV_FILE=$(ENV) scripts/create_commit_pool.sh "$(NAME)" "$(SYMBOL)"

# Full automated end-to-end rehearsal against the deployed testnet stack.
lifecycle-test:
	ENV_FILE=$(ENV) scripts/run_lifecycle_test.sh

.PHONY: help build test optimize optimize-all optimize-creator-pool optimize-factory optimize-router \
	check check-pool check-factory deploy status pool lifecycle-test
