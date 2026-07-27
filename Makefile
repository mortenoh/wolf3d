# Wolfenstein 3D / Spear of Destiny in Rust.
#
# The game data (.WL6 / .SOD / .SD2 / .SD3) is copyrighted and never committed.
# `make data` extracts Wolfenstein 3D from its GOG installer; `make data-sod`
# extracts Spear of Destiny mission 1; `make data-sod-m2` / `data-sod-m3` extract
# mission packs 2 and 3. Place (or keep) the installers at
# data/Wolfenstein.3D.and.Spear.of.Destiny-GOG/setup_wolfenstein3d_*.exe and
# .../setup_spear_of_destiny_*.exe respectively.
# Requires innoextract (brew install innoextract / apt install innoextract).
#
# Run `make` or `make help` for the target list.

CARGO      ?= cargo
DATA_DIR   := data
GOG_DIR    := $(DATA_DIR)/Wolfenstein.3D.and.Spear.of.Destiny-GOG
# Deferred (not :=) so the wildcard is evaluated when a data target runs,
# rather than once at parse time.
SETUP       = $(wildcard $(GOG_DIR)/setup_wolfenstein3d_*.exe)
SETUP_SOD   = $(wildcard $(GOG_DIR)/setup_spear_of_destiny_*.exe)

# macOS universal binary (arm64 + x86_64), same idea as mortenoh/macgames.
# Non-Darwin hosts just install the host release binary.
UNAME_S    := $(shell uname -s)
LIPO       ?= lipo
MAC_ARM    := aarch64-apple-darwin
MAC_X64    := x86_64-apple-darwin
BIN_DIR    := bin
BIN_NAME   := wolf3d

# Optional knobs for the run/demo targets:
#   make run LEVEL=5                      start on level 5 (WOLF3D_LEVEL)
#   make record OUT=demos/x.dm            capture a session as an attract demo
#   make demo SCRIPT='w:1;use;snap:door'  headless scripted run, dumps snapshots
#   make play-demo DEMO=e1m1              watch a .dm (DEMO=path or bare stem)
#   make play-demo DEMO=e1m1 HEADLESS=1   same, no window (exit when finished)
LEVEL      ?=
OUT        ?= demos/recorded.dm
SCRIPT     ?=
DEMO       ?=
SNAP_DIR   ?= snaps
HEADLESS   ?=

.DEFAULT_GOAL := help

.PHONY: help run run-sod run-sod-m2 run-sod-m3 record demo play-demo gen-demos gen-demo \
        build build-release release ensure-mac-targets \
        check fmt fmt-check clippy test test-compile ci data data-sod \
        data-sod-m2 data-sod-m3 data-all clean clean-data clean-saves \
        clean-snaps distclean

help: ## Show this help
	@awk 'BEGIN { FS = ":.*##" } \
	     /^##@/ { printf "\n%s\n", substr($$0, 5); next } \
	     /^[a-zA-Z0-9_-]+:.*##/ { printf "  %-16s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""

##@ Play

RUN_BIN := $(CARGO) run --release --bin wolf3d

run: data ## Play Wolfenstein 3D (LEVEL=n starts on that level)
	$(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(RUN_BIN)

run-sod: data-sod ## Play Spear of Destiny M1 (LEVEL=n starts on that level)
	WOLF3D_GAME=sod $(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(RUN_BIN)

run-sod-m2: data-sod-m2 ## Play Spear mission pack 2 (Return to Danger)
	WOLF3D_GAME=sd2 $(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(RUN_BIN)

run-sod-m3: data-sod-m3 ## Play Spear mission pack 3 (Ultimate Challenge)
	WOLF3D_GAME=sd3 $(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(RUN_BIN)

record: data ## Record the next play session as an attract demo (OUT=path)
	WOLF3D_RECORD=$(OUT) $(RUN_BIN)

demo: data ## Run a headless input script (SCRIPT='w:1;use;snap:door')
	@test -n "$(SCRIPT)" || { echo "SCRIPT is required, e.g. make demo SCRIPT='w:1;use;snap:door'"; exit 1; }
	WOLF3D_DEMO="$(SCRIPT)" WOLF3D_SNAP_DIR=$(SNAP_DIR) $(RUN_BIN)

play-demo: data ## Replay a .dm attract demo (DEMO=e1m1 or demos/e1m1.dm; HEADLESS=1 for no window)
	@test -n "$(DEMO)" || { echo "DEMO is required, e.g. make play-demo DEMO=e1m1"; exit 1; }
	WOLF3D_PLAY_DEMO="$(DEMO)" $(if $(filter 1,$(HEADLESS)),WOLF3D_HEADLESS=1 )$(RUN_BIN)

# AI demo forge via `wolf3d forge`. Example: make gen-demo LEVEL=0 ITERS=1000000
ITERS    ?= 50000
THREADS  ?=
MAX_SECS ?= 120
GOD      ?=
FOCUS    ?= secrets

gen-demos: data ## AI-search fair demos for every WL6 floor (wolf3d forge)
	$(CARGO) run --release --bin wolf3d -- forge --iters $(ITERS) \
		$(if $(THREADS),--threads $(THREADS)) --max-secs $(MAX_SECS) \
		$(if $(filter 1,$(GOD)),--god) --focus $(FOCUS)

gen-demo: data ## AI-search one floor (LEVEL=0-based index, ITERS=n)
	@test -n "$(LEVEL)" || { echo "LEVEL is required (0-based), e.g. make gen-demo LEVEL=0 ITERS=100000"; exit 1; }
	$(CARGO) run --release --bin wolf3d -- forge --levels $(LEVEL) --iters $(ITERS) \
		$(if $(THREADS),--threads $(THREADS)) --max-secs $(MAX_SECS) \
		$(if $(filter 1,$(GOD)),--god) --focus $(FOCUS)

##@ Develop

build: ## Build the debug binary
	$(CARGO) build

build-release: ## Build the host release binary (target/release/wolf3d)
	$(CARGO) build --release --bin $(BIN_NAME)

# Ensure Rust targets exist (no-op if already installed).
ensure-mac-targets:
	@rustup target list --installed | grep -qx '$(MAC_ARM)' \
		|| rustup target add $(MAC_ARM)
	@rustup target list --installed | grep -qx '$(MAC_X64)' \
		|| rustup target add $(MAC_X64)

ifeq ($(UNAME_S),Darwin)
release: ensure-mac-targets ## Universal bin/wolf3d (arm64 + x86_64 via lipo)
	$(CARGO) build --release --bin $(BIN_NAME) --target $(MAC_ARM)
	$(CARGO) build --release --bin $(BIN_NAME) --target $(MAC_X64)
	mkdir -p $(BIN_DIR)
	$(LIPO) -create \
		target/$(MAC_ARM)/release/$(BIN_NAME) \
		target/$(MAC_X64)/release/$(BIN_NAME) \
		-output $(BIN_DIR)/$(BIN_NAME)
	@echo "Installed $(BIN_DIR)/$(BIN_NAME) (universal):"
	@$(LIPO) -info $(BIN_DIR)/$(BIN_NAME)
else
release: ## Host release binary -> bin/wolf3d
	$(CARGO) build --release --bin $(BIN_NAME)
	mkdir -p $(BIN_DIR)
	cp -f target/release/$(BIN_NAME) $(BIN_DIR)/$(BIN_NAME)
	@echo "Installed $(BIN_DIR)/$(BIN_NAME)"
endif

check: ## Type-check without producing binaries
	$(CARGO) check --all-targets

fmt: ## Format the source
	$(CARGO) fmt

fmt-check: ## Verify formatting (as CI does)
	$(CARGO) fmt --check

clippy: ## Lint with warnings denied (as CI does)
	$(CARGO) clippy --all-targets -- -D warnings

test: data ## Run the test suite (needs the extracted game data)
	$(CARGO) test

test-compile: ## Compile the tests without running them (as CI does)
	$(CARGO) test --no-run

ci: fmt-check clippy build-release test-compile ## Run everything CI runs

##@ Game data

data: $(DATA_DIR)/VSWAP.WL6 ## Extract Wolfenstein 3D from the GOG installer

data-sod: $(DATA_DIR)/VSWAP.SOD ## Extract Spear of Destiny M1 (*.SOD)

data-sod-m2: $(DATA_DIR)/VSWAP.SD2 ## Extract Spear mission pack 2 (*.SD2)

data-sod-m3: $(DATA_DIR)/VSWAP.SD3 ## Extract Spear mission pack 3 (*.SD3)

data-all: data data-sod data-sod-m2 data-sod-m3 ## Extract WL6 + all Spear packs

$(DATA_DIR)/VSWAP.WL6:
	@command -v innoextract >/dev/null || { echo "innoextract not found: brew install innoextract"; exit 1; }
	@test -n "$(SETUP)" || { echo "GOG installer not found in $(GOG_DIR)/"; exit 1; }
	rm -rf $(DATA_DIR)/.extract
	innoextract -s -d $(DATA_DIR)/.extract "$(SETUP)"
	cp $(DATA_DIR)/.extract/app/*.WL6 $(DATA_DIR)/
	rm -rf $(DATA_DIR)/.extract

# Spear of Destiny GOG installer ships three packs under app/M1, app/M2, app/M3.
# Extract once into .extract-sod; each pack target copies its folder into data/
# with a pack-specific extension (M1→.SOD, M2→.SD2, M3→.SD3) so they coexist.
$(DATA_DIR)/.extract-sod/.done:
	@command -v innoextract >/dev/null || { echo "innoextract not found: brew install innoextract"; exit 1; }
	@test -n "$(SETUP_SOD)" || { echo "SOD GOG installer not found in $(GOG_DIR)/"; exit 1; }
	rm -rf $(DATA_DIR)/.extract-sod
	innoextract -s -d $(DATA_DIR)/.extract-sod "$(SETUP_SOD)"
	@test -d $(DATA_DIR)/.extract-sod/app/M1 || { echo "SOD extract missing app/M1"; exit 1; }
	touch $(DATA_DIR)/.extract-sod/.done

$(DATA_DIR)/VSWAP.SOD: $(DATA_DIR)/.extract-sod/.done
	cp $(DATA_DIR)/.extract-sod/app/M1/*.SOD $(DATA_DIR)/

$(DATA_DIR)/VSWAP.SD2: $(DATA_DIR)/.extract-sod/.done
	@test -d $(DATA_DIR)/.extract-sod/app/M2 || { echo "SOD extract missing app/M2"; exit 1; }
	for f in $(DATA_DIR)/.extract-sod/app/M2/*.SOD; do \
		base=$$(basename "$$f" .SOD); \
		cp "$$f" "$(DATA_DIR)/$$base.SD2"; \
	done

$(DATA_DIR)/VSWAP.SD3: $(DATA_DIR)/.extract-sod/.done
	@test -d $(DATA_DIR)/.extract-sod/app/M3 || { echo "SOD extract missing app/M3"; exit 1; }
	for f in $(DATA_DIR)/.extract-sod/app/M3/*.SOD; do \
		base=$$(basename "$$f" .SOD); \
		cp "$$f" "$(DATA_DIR)/$$base.SD3"; \
	done

##@ Clean

clean: ## Remove build artifacts
	$(CARGO) clean

clean-data: ## Remove the extracted game data (keeps the GOG installers)
	rm -rf $(DATA_DIR)/.extract $(DATA_DIR)/.extract-sod
	rm -f $(DATA_DIR)/*.WL6 $(DATA_DIR)/*.SOD $(DATA_DIR)/*.SD2 $(DATA_DIR)/*.SD3

clean-saves: ## Remove save slots, config, and high scores
	rm -rf saves

clean-snaps: ## Remove headless demo snapshots
	rm -rf $(SNAP_DIR)

distclean: clean clean-data clean-saves clean-snaps ## Remove everything generated
