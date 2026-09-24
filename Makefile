BIN ?= $(HOME)/.local/bin
PLUGIN := $(CURDIR)/plugin
LABEL = $(shell $(BIN)/alondra config --get service_label 2>/dev/null)

.PHONY: no-holds build test install run tui setup launchd unlaunchd restart status doctor cargo-test mac e2e-sandbox soak

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
	@echo "Installed in $(BIN). Plugin: $(PLUGIN)"

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

no-holds:
	@holds=$$(curl -s --max-time 3 http://127.0.0.1:47800/v1/health | sed -n 's/.*"holds":\([0-9]*\).*/\1/p'); \
	if [ -n "$$holds" ] && [ "$$holds" != "0" ] && [ -z "$(FORCE)" ]; then \
		echo "alondrad is holding $$holds question(s) or permission(s) for nodes; restarting would cut them. Answer them first or run with FORCE=1."; exit 1; \
	fi

restart: no-holds
	launchctl kickstart -k gui/$$(id -u)/$(LABEL)

launchd: no-holds install
	$(BIN)/alondra setup --bin $(BIN) --skip-plugin --skip-pair

unlaunchd:
	launchctl bootout gui/$$(id -u)/$(LABEL) 2>/dev/null || true
	rm -f $(HOME)/Library/LaunchAgents/$(LABEL).plist

mac:
	bazelisk build //apple:AlondraMac
	-osascript -e 'if application "$(HOME)/Applications/Alondra.app" is running then tell application "$(HOME)/Applications/Alondra.app" to quit'
	rm -rf $(HOME)/Applications/Alondra.app
	mkdir -p $(HOME)/Applications
	unzip -q -o $$(bazelisk cquery --output=files //apple:AlondraMac 2>/dev/null | grep '\.zip$$' | head -1) -d $(HOME)/Applications
	open $(HOME)/Applications/Alondra.app


e2e-sandbox:
	bazelisk build //crates/alondrad //crates/alondra
	tools/e2e-sandbox.sh $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null) $$(bazelisk cquery --output=files //crates/alondra 2>/dev/null)

soak:
	bazelisk build //crates/alondrad
	taskpolicy -b python3 tools/soak.py --binary $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null) $(SOAK_ARGS)
