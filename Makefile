# The game data (.WL6) is copyrighted and never committed. `make data`
# extracts it from the GOG installer, which you must place (or keep) at
# data/Wolfenstein.3D.and.Spear.of.Destiny-GOG/setup_wolfenstein3d_*.exe.
# Requires innoextract (brew install innoextract).

DATA_DIR := data
GOG_DIR  := $(DATA_DIR)/Wolfenstein.3D.and.Spear.of.Destiny-GOG
SETUP    := $(wildcard $(GOG_DIR)/setup_wolfenstein3d_*.exe)

.PHONY: run test data clean

run: data
	cargo run --release

test: data
	cargo test

data: $(DATA_DIR)/VSWAP.WL6

$(DATA_DIR)/VSWAP.WL6:
	@command -v innoextract >/dev/null || { echo "innoextract not found: brew install innoextract"; exit 1; }
	@test -n "$(SETUP)" || { echo "GOG installer not found in $(GOG_DIR)/"; exit 1; }
	innoextract -s -d $(DATA_DIR)/.extract "$(SETUP)"
	cp $(DATA_DIR)/.extract/app/*.WL6 $(DATA_DIR)/
	rm -rf $(DATA_DIR)/.extract

clean:
	cargo clean
