# ─── Config ───────────────────────────────────────────────────────────────────

WASM_TARGET       := wasm32-unknown-unknown
ARTIFACTS         := artifacts

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

# ─── Chain Deploy ────────────────────────────────────────────────────────────
deploy-all-local:
	@echo "For a full chain deployment: ./deploy_osmosis.sh <env-file>"

.PHONY: build test optimize optimize-all optimize-creator-pool optimize-factory optimize-router \
	check check-pool check-factory deploy-all-local
