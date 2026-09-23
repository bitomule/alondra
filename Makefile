BIN ?= $(HOME)/.local/bin
PLUGIN := $(CURDIR)/plugin
PLIST := $(HOME)/Library/LaunchAgents/com.bitomule.alondra.plist

.PHONY: build test install run tui launchd unlaunchd restart status cargo-test mac

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

restart:
	launchctl kickstart -k gui/$$(id -u)/com.bitomule.alondra

launchd: install
	sed -e 's|@BIN@|$(BIN)|g' -e 's|@HOME@|$(HOME)|g' -e 's|@PLUGIN@|$(PLUGIN)|g' launchd/com.bitomule.alondra.plist.in > $(PLIST)
	launchctl bootout gui/$$(id -u) $(PLIST) 2>/dev/null || true
	-pkill -f "$(BIN)/alondrad$$"
	sleep 1
	launchctl bootstrap gui/$$(id -u) $(PLIST)
	@echo "alondrad arranca solo al iniciar sesión y se reinicia si cae. Registro: ~/.alondra/alondrad.log"

unlaunchd:
	launchctl bootout gui/$$(id -u) $(PLIST) 2>/dev/null || true
	rm -f $(PLIST)

mac:
	bazelisk build //apple:AlondraMac
	rm -rf $(HOME)/Applications/Alondra.app
	mkdir -p $(HOME)/Applications
	unzip -q -o $$(bazelisk cquery --output=files //apple:AlondraMac 2>/dev/null | grep '\.zip$$' | head -1) -d $(HOME)/Applications
	open $(HOME)/Applications/Alondra.app
