# The game data (.WL6 / .SOD) is copyrighted and never committed. `make data`
# extracts Wolfenstein 3D from its GOG installer and `make data-sod` extracts
# Spear of Destiny from its GOG installer; place (or keep) the installers at
# data/Wolfenstein.3D.and.Spear.of.Destiny-GOG/setup_wolfenstein3d_*.exe and
# .../setup_spear_of_destiny_*.exe respectively.
# Requires innoextract (brew install innoextract).

DATA_DIR   := data
GOG_DIR    := $(DATA_DIR)/Wolfenstein.3D.and.Spear.of.Destiny-GOG
SETUP      := $(wildcard $(GOG_DIR)/setup_wolfenstein3d_*.exe)
SETUP_SOD  := $(wildcard $(GOG_DIR)/setup_spear_of_destiny_*.exe)

.PHONY: run test data data-sod clean

run: data
	cargo run --release

# Play Spear of Destiny (needs `make data-sod` first).
run-sod: data-sod
	WOLF3D_GAME=sod cargo run --release

test: data
	cargo test

data: $(DATA_DIR)/VSWAP.WL6

$(DATA_DIR)/VSWAP.WL6:
	@command -v innoextract >/dev/null || { echo "innoextract not found: brew install innoextract"; exit 1; }
	@test -n "$(SETUP)" || { echo "GOG installer not found in $(GOG_DIR)/"; exit 1; }
	innoextract -s -d $(DATA_DIR)/.extract "$(SETUP)"
	cp $(DATA_DIR)/.extract/app/*.WL6 $(DATA_DIR)/
	rm -rf $(DATA_DIR)/.extract

# Spear of Destiny. The GOG installer ships three mission packs under app/M1,
# app/M2, app/M3; M1 is the original Spear of Destiny campaign, which we use.
data-sod: $(DATA_DIR)/VSWAP.SOD

$(DATA_DIR)/VSWAP.SOD:
	@command -v innoextract >/dev/null || { echo "innoextract not found: brew install innoextract"; exit 1; }
	@test -n "$(SETUP_SOD)" || { echo "SOD GOG installer not found in $(GOG_DIR)/"; exit 1; }
	innoextract -s -d $(DATA_DIR)/.extract-sod "$(SETUP_SOD)"
	cp $(DATA_DIR)/.extract-sod/app/M1/*.SOD $(DATA_DIR)/
	rm -rf $(DATA_DIR)/.extract-sod

clean:
	cargo clean
