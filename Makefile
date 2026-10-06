BIN ?= $(HOME)/.local/bin
PLUGIN := $(CURDIR)/plugin
LABEL = $(shell $(BIN)/alondra config --get service_label 2>/dev/null)
DEVICE ?= David iPhone
export DEVICE

.PHONY: snapshots-record snapshots-verify mac-snapshots-record mac-snapshots-verify no-holds build test install run tui setup launchd unlaunchd restart status doctor cargo-test mac device device-console e2e-sandbox e2e-sandbox-stop e2e-sandbox-logs soak tasks-screen rehearse-cutover cutover-live

build:
	bazelisk build //crates/... //apple:AlondraMac

# Same build/install/launch flow as Quill, Lumo and Memoir.
# Override with make device DEVICE="device name or UDID".
device:
	python3 tools/device.py

# The iPhone's unified log does not reach this Mac; only `--console` brings the
# app's stdout back, so traces print to stdout and this target reads them. Device
# builds are `-c opt`: a trace behind `#if DEBUG` never reaches this console.
device-console: device
	xcrun devicectl device process launch --console --terminate-existing --device "$(DEVICE)" com.bitomule.alondra

test:
	bazelisk test //crates/...

SNAPSHOT_TARGET := //apple/Testing/Snapshots:AlondraSnapshotTests
SNAPSHOT_ROOT := apple/Testing/Snapshots/Tests/__Snapshots__

# Record mode always reports FAILED; the images land in the test outputs and are copied back.
snapshots-record:
	-bazelisk test $(SNAPSHOT_TARGET) \
		--test_env=SNAPSHOT_TESTING_RECORD=all \
		--test_env=SNAPSHOT_ROOT=$(SNAPSHOT_ROOT) \
		--nocache_test_results --test_output=errors
	@outputs=$$(find bazel-testlogs/apple/Testing/Snapshots/AlondraSnapshotTests -type d -name test.outputs | head -1); \
	if [ -n "$$outputs" ] && [ -d "$$outputs/$(SNAPSHOT_ROOT)" ]; then \
		mkdir -p $(SNAPSHOT_ROOT) && cp -R "$$outputs/$(SNAPSHOT_ROOT)/." $(SNAPSHOT_ROOT)/ && chmod -R u+w $(SNAPSHOT_ROOT); \
		echo "Baselines copied to $(SNAPSHOT_ROOT)"; \
	else \
		echo "No recorded snapshots found"; exit 1; \
	fi

snapshots-verify:
	bazelisk test $(SNAPSHOT_TARGET) --test_output=errors

MAC_SNAPSHOT_TARGET := //apple/Testing/MacSnapshots:AlondraMacSnapshotTests
MAC_SNAPSHOT_ROOT := apple/Testing/MacSnapshots/Tests/__Snapshots__

mac-snapshots-record:
	-bazelisk test $(MAC_SNAPSHOT_TARGET) \
		--test_env=SNAPSHOT_TESTING_RECORD=all \
		--test_env=SNAPSHOT_ROOT=$(MAC_SNAPSHOT_ROOT) \
		--nocache_test_results --test_output=errors
	@outputs=$$(find bazel-testlogs/apple/Testing/MacSnapshots/AlondraMacSnapshotTests -type d -name test.outputs | head -1); \
	if [ -n "$$outputs" ] && [ -d "$$outputs/$(MAC_SNAPSHOT_ROOT)" ]; then \
		mkdir -p $(MAC_SNAPSHOT_ROOT) && cp -R "$$outputs/$(MAC_SNAPSHOT_ROOT)/." $(MAC_SNAPSHOT_ROOT)/ && chmod -R u+w $(MAC_SNAPSHOT_ROOT); \
		echo "Baselines copied to $(MAC_SNAPSHOT_ROOT)"; \
	else \
		echo "No recorded snapshots found"; exit 1; \
	fi

mac-snapshots-verify:
	bazelisk test $(MAC_SNAPSHOT_TARGET) --test_output=errors

cargo-test:
	cargo test

brew-guard:
	@if [ -z "$(FORCE_DEV_INSTALL)" ] && { [ -e /opt/homebrew/opt/alondra ] || [ -e /usr/local/opt/alondra ]; }; then \
		echo "Alondra is installed with Homebrew; dev installs would replace the user's daemon. Use make e2e-sandbox, or FORCE_DEV_INSTALL=1 if you really mean it"; exit 1; \
	fi

install: brew-guard build
	mkdir -p $(BIN) $(PLUGIN)/bin $(HOME)/.alondra
	install -m 755 $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null) $(BIN)/alondrad
	install -m 755 $$(bazelisk cquery --output=files //crates/alondra 2>/dev/null) $(BIN)/alondra
	@echo "Installed in $(BIN). Plugin: $(PLUGIN)"

run: brew-guard install
	$(BIN)/alondrad

tui:
	$(BIN)/alondra

status:
	$(BIN)/alondra status

doctor:
	$(BIN)/alondra doctor

setup: brew-guard install
	$(BIN)/alondra setup --bin $(BIN) --repo $(CURDIR)

no-holds:
	@holds=$$(curl -s --max-time 3 http://127.0.0.1:47800/v1/health | sed -n 's/.*"holds":\([0-9]*\).*/\1/p'); \
	if [ -n "$$holds" ] && [ "$$holds" != "0" ] && [ -z "$(FORCE)" ]; then \
		echo "alondrad is holding $$holds question(s) or permission(s) for nodes; restarting would cut them. Answer them first or run with FORCE=1."; exit 1; \
	fi

restart: no-holds
	launchctl kickstart -k gui/$$(id -u)/$(LABEL)

launchd: brew-guard no-holds install
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


# MACS=n daemons in service mode with a loopback relay and a synthetic phone (tools/e2e-sandbox.sh).
e2e-sandbox:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e-sandbox.sh $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null) $$(bazelisk cquery --output=files //crates/alondra 2>/dev/null) \
		$$(bazelisk cquery --output=files //crates/relay:alondra-relay 2>/dev/null) $$(bazelisk cquery --output=files //tools/sandbox:alondra-sandbox 2>/dev/null)

e2e-sandbox-stop:
	tools/e2e-sandbox-stop.sh

# MAC=n for that daemon's log, MAC=relay for the relay's.
e2e-sandbox-logs:
	@dir=$${SANDBOX_DIR:-/tmp/alondra-sandbox}; \
	if [ "$(MAC)" = relay ]; then tail -f $$dir/relay.log; else tail -f $$dir/mac-$${MAC:-1}/alondrad.log; fi

# Full cutover rehearsal on a read-only snapshot of the live HOME; keeps only the report.
# REHEARSAL_ROOT, LIVE_HOME and LEGACY override the defaults (tools/rehearse-cutover.sh).
rehearse-cutover:
	bazelisk build //crates/alondrad
	tools/rehearse-cutover.sh $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null)

# The one-command cutover of the live HOME to domain v2 (docs/domain-v2-live-cutover.md).
# BACKUP_ROOT, LEGACY_DAEMON and LEGACY_CLI override the defaults (tools/cutover-live.sh).
cutover-live:
	bazelisk build //crates/alondrad
	tools/cutover-live.sh $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null)

soak:
	bazelisk build //crates/alondrad
	taskpolicy -b python3 tools/soak.py --binary $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null) $(SOAK_ARGS)

# The TUI's Tasks tab for the running alondrad's overview, at 130x33, as text (SCREEN=path keeps it).
tasks-screen:
	curl -sf http://127.0.0.1:47800/v1/overview -o /tmp/alondra-overview.json
	ALONDRA_OVERVIEW=/tmp/alondra-overview.json ALONDRA_SCREEN=$(or $(SCREEN),/tmp/alondra-tasks.txt) cargo test -q -p alondra tasks_screen_of_an_overview -- --ignored
	cat $(or $(SCREEN),/tmp/alondra-tasks.txt)
