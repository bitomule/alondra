BIN ?= $(HOME)/.local/bin
PLUGIN := $(CURDIR)/plugin
LABEL = $(shell $(BIN)/alondra config --get service_label 2>/dev/null)

.PHONY: build test install run tui setup launchd unlaunchd restart status doctor cargo-test mac

build:
	bazelisk build //crates/... //apple:AlondraMac

test:
	bazelisk test //crates/...

cargo-test:
	cargo test

install: build
	mkdir -p $(BIN) $(PLUGIN)/bin $(HOME)/.alondra
	install -m 755 $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null) $(BIN)/alondrad
	install -m 755 $$(bazelisk cquery --output=files //crates/alondra 2>/dev/null) $(BIN)/alondra
	@echo "Instalado en $(BIN). Plugin: $(PLUGIN)"

run: install
	$(BIN)/alondrad

tui:
	$(BIN)/alondra

status:
	$(BIN)/alondra status

doctor:
	$(BIN)/alondra doctor

setup: install
	$(BIN)/alondra setup --bin $(BIN) --repo $(CURDIR)

restart:
	launchctl kickstart -k gui/$$(id -u)/$(LABEL)

launchd: install
	$(BIN)/alondra setup --bin $(BIN) --skip-plugin --skip-pair

unlaunchd:
	launchctl bootout gui/$$(id -u)/$(LABEL) 2>/dev/null || true
	rm -f $(HOME)/Library/LaunchAgents/$(LABEL).plist

mac:
	bazelisk build //apple:AlondraMac
	rm -rf $(HOME)/Applications/Alondra.app
	mkdir -p $(HOME)/Applications
	unzip -q -o $$(bazelisk cquery --output=files //apple:AlondraMac 2>/dev/null | grep '\.zip$$' | head -1) -d $(HOME)/Applications
	open $(HOME)/Applications/Alondra.app
