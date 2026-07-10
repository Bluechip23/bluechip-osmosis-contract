# ─── Config ───────────────────────────────────────────────────────────────────

# Local chain config
LOCAL_CHAIN_ID    := bluechipChain
LOCAL_KEYRING     := test
LOCAL_FROM        := alice
LOCAL_CHAIN_BIN   := bluechipChaind
LOCAL_GAS         := 5000000
LOCAL_GAS_ADJ     := 1.3

# Sei testnet config
SEI_NODE          := https://rpc-testnet.sei-apis.com
SEI_CHAIN_ID      := atlantic-2
SEI_FROM          := taku

# Common
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
	cp target/$(WASM_TARGET)/release/standard_pool.wasm $(ARTIFACTS)/standard_pool.wasm
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

optimize-standard-pool:
	docker run --rm -v ${CURDIR}:/code \
	  --mount type=volume,source=standard_pool_cache,target=/target \
	  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
	  cosmwasm/optimizer:0.16.0 ./standard-pool

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

optimize-all: optimize-creator-pool optimize-standard-pool optimize-factory optimize-router

# ─── Cosmwasm Check ──────────────────────────────────────────────────────────
check:
	cosmwasm-check $(ARTIFACTS)/creator_pool.wasm
	cosmwasm-check $(ARTIFACTS)/standard_pool.wasm
	cosmwasm-check $(ARTIFACTS)/factory.wasm
	cosmwasm-check $(ARTIFACTS)/router.wasm

check-pool:
	cosmwasm-check $(ARTIFACTS)/creator_pool.wasm
	cosmwasm-check $(ARTIFACTS)/standard_pool.wasm

check-factory:
	cosmwasm-check $(ARTIFACTS)/factory.wasm

# ─── Local Chain Deploy (store wasm on local chain) ──────────────────────────
deploy-pool-local: build
	@echo "Deploying creator-pool contract to local chain..."
	$(LOCAL_CHAIN_BIN) tx wasm store $(ARTIFACTS)/creator_pool.wasm \
		--from $(LOCAL_FROM) \
		--chain-id $(LOCAL_CHAIN_ID) \
		--gas $(LOCAL_GAS) --gas-adjustment $(LOCAL_GAS_ADJ) \
		--keyring-backend $(LOCAL_KEYRING) \
		-y --output json

deploy-standard-pool-local: build
	@echo "Deploying standard-pool contract to local chain..."
	$(LOCAL_CHAIN_BIN) tx wasm store $(ARTIFACTS)/standard_pool.wasm \
		--from $(LOCAL_FROM) \
		--chain-id $(LOCAL_CHAIN_ID) \
		--gas $(LOCAL_GAS) --gas-adjustment $(LOCAL_GAS_ADJ) \
		--keyring-backend $(LOCAL_KEYRING) \
		-y --output json

deploy-factory-local: build
	@echo "Deploying factory contract to local chain..."
	$(LOCAL_CHAIN_BIN) tx wasm store $(ARTIFACTS)/factory.wasm \
		--from $(LOCAL_FROM) \
		--chain-id $(LOCAL_CHAIN_ID) \
		--gas $(LOCAL_GAS) --gas-adjustment $(LOCAL_GAS_ADJ) \
		--keyring-backend $(LOCAL_KEYRING) \
		-y --output json

# ─── Full Stack Local Deploy ─────────────────────────────────────────────────
deploy-all-local:
	@echo "For a full chain deployment: ./deploy_osmosis.sh <env-file>"

# ─── Sei Testnet Deploy ─────────────────────────────────────────────────────
deploy-pool: optimize-pool
	@echo "Deploying creator-pool contract to Sei testnet..."
	seid tx wasm store artifacts/creator_pool.wasm \
		--from $(SEI_FROM) \
		--node $(SEI_NODE) \
		--chain-id $(SEI_CHAIN_ID) \
		-b block \
		--gas 5000000 \
		--fees 300000usei \
		-y | tee ./config/pool_deploy_result.txt

deploy-standard-pool: optimize-pool
	@echo "Deploying standard-pool contract to Sei testnet..."
	seid tx wasm store artifacts/standard_pool.wasm \
		--from $(SEI_FROM) \
		--node $(SEI_NODE) \
		--chain-id $(SEI_CHAIN_ID) \
		-b block \
		--gas 5000000 \
		--fees 300000usei \
		-y | tee ./config/standard_pool_deploy_result.txt

deploy-factory: optimize-factory
	@echo "Deploying factory contract to Sei testnet..."
	seid tx wasm store artifacts/factory.wasm \
		--from $(SEI_FROM) \
		--node $(SEI_NODE) \
		--chain-id $(SEI_CHAIN_ID) \
		-b block \
		--gas 5000000 \
		--fees 300000usei \
		-y | tee ./config/factory_deploy_result.txt

init-factory:
	seid tx wasm instantiate 10234 "$$(cat ./config/factory_init.json)" \
		--from $(SEI_FROM) \
		--label "bluechip_factory" \
		--admin $$(seid keys show $(SEI_FROM) -a) \
		--node $(SEI_NODE) \
		--chain-id $(SEI_CHAIN_ID) \
		--gas 5000000 \
		--fees 300000usei \
		-b block \
		-y | tee ./config/factory_init_result.txt

init-pool:
	seid tx wasm instantiate 10236 "$$(cat ./config/pool_init.json)" \
		--from $(SEI_FROM) \
		--label "bluechip_pool" \
		--admin $$(seid keys show $(SEI_FROM) -a) \
		--node $(SEI_NODE) \
		--chain-id $(SEI_CHAIN_ID) \
		--gas 5000000 \
		--fees 300000usei \
		-b block \
		-y | tee ./config/pool_init_result.txt

.PHONY: build test optimize optimize-all optimize-creator-pool optimize-standard-pool optimize-factory optimize-router \
	check check-pool check-factory \
	deploy-pool-local deploy-factory-local deploy-all-local \
