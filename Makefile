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

# Optional knobs for the run/demo targets:
#   make run LEVEL=5                      start on level 5 (WOLF3D_LEVEL)
#   make record OUT=demos/x.dm            capture a session as an attract demo
#   make demo SCRIPT='w:1;use;snap:door'  headless scripted run, dumps snapshots
LEVEL      ?=
OUT        ?= demos/recorded.dm
SCRIPT     ?=
SNAP_DIR   ?= snaps

.DEFAULT_GOAL := help

.PHONY: help run run-sod run-sod-m2 run-sod-m3 record demo gen-demos \
        build build-release \
        check fmt fmt-check clippy test test-compile ci data data-sod \
        data-sod-m2 data-sod-m3 data-all clean clean-data clean-saves \
        clean-snaps distclean

help: ## Show this help
	@awk 'BEGIN { FS = ":.*##" } \
	     /^##@/ { printf "\n%s\n", substr($$0, 5); next } \
	     /^[a-zA-Z0-9_-]+:.*##/ { printf "  %-16s %s\n", $$1, $$2 }' $(MAKEFILE_LIST)
	@echo ""

##@ Play

run: data ## Play Wolfenstein 3D (LEVEL=n starts on that level)
	$(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(CARGO) run --release

run-sod: data-sod ## Play Spear of Destiny M1 (LEVEL=n starts on that level)
	WOLF3D_GAME=sod $(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(CARGO) run --release

run-sod-m2: data-sod-m2 ## Play Spear mission pack 2 (Return to Danger)
	WOLF3D_GAME=sd2 $(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(CARGO) run --release

run-sod-m3: data-sod-m3 ## Play Spear mission pack 3 (Ultimate Challenge)
	WOLF3D_GAME=sd3 $(if $(LEVEL),WOLF3D_LEVEL=$(LEVEL) )$(CARGO) run --release

record: data ## Record the next play session as an attract demo (OUT=path)
	WOLF3D_RECORD=$(OUT) $(CARGO) run --release

demo: data ## Run a headless input script (SCRIPT='w:1;use;snap:door')
	@test -n "$(SCRIPT)" || { echo "SCRIPT is required, e.g. make demo SCRIPT='w:1;use;snap:door'"; exit 1; }
	WOLF3D_DEMO="$(SCRIPT)" WOLF3D_SNAP_DIR=$(SNAP_DIR) $(CARGO) run --release

gen-demos: data ## Generate attract demos that complete every WL6 floor (demos/eXmY.dm)
	$(CARGO) run --release --bin gen_level_demos

##@ Develop

build: ## Build the debug binary
	$(CARGO) build

build-release: ## Build the release binary
	$(CARGO) build --release

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
