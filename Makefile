BIN ?= $(HOME)/.local/bin
PLUGIN := $(CURDIR)/plugin
PLIST := $(HOME)/Library/LaunchAgents/com.bitomule.alondra.plist

.PHONY: build test install run tui launchd unlaunchd status cargo-test mac

build:
	bazelisk build //crates/... //apple:AlondraMac

test:
	bazelisk test //crates/...

cargo-test:
	cargo test

install: build
	mkdir -p $(BIN) $(PLUGIN)/bin $(HOME)/.alondra
	install -m 755 bazel-bin/crates/alondrad/alondrad $(BIN)/alondrad
	install -m 755 bazel-bin/crates/alondra/alondra $(BIN)/alondra
	ln -sf $(BIN)/alondra $(PLUGIN)/bin/alondra
	@echo "Instalado en $(BIN). Plugin: $(PLUGIN)"

run: install
	ALONDRA_PLUGIN_DIR=$(PLUGIN) $(BIN)/alondrad

tui:
	$(BIN)/alondra

status:
	$(BIN)/alondra status

launchd: install
	sed -e 's|@BIN@|$(BIN)|g' -e 's|@HOME@|$(HOME)|g' -e 's|@PLUGIN@|$(PLUGIN)|g' launchd/com.bitomule.alondra.plist.in > $(PLIST)
	launchctl bootout gui/$$(id -u) $(PLIST) 2>/dev/null || true
	launchctl bootstrap gui/$$(id -u) $(PLIST)
	@echo "alondrad arranca solo al iniciar sesión y se reinicia si cae. Registro: ~/.alondra/alondrad.log"

unlaunchd:
	launchctl bootout gui/$$(id -u) $(PLIST) 2>/dev/null || true
	rm -f $(PLIST)

mac:
	bazelisk build //apple:AlondraMac
	rm -rf $(HOME)/Applications/Alondra.app
	mkdir -p $(HOME)/Applications
	unzip -q -o bazel-bin/apple/AlondraMac.zip -d $(HOME)/Applications
	open $(HOME)/Applications/Alondra.app
