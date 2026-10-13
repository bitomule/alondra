BIN ?= $(HOME)/.local/bin
PLUGIN := $(CURDIR)/plugin
LABEL = $(shell $(BIN)/alondra config get service_label 2>/dev/null)
DEVICE ?= David iPhone
# A Rust target's executable: cquery also lists the clippy report it builds beside it.
exe = $$(bazelisk cquery --output=files $(1) 2>/dev/null | grep -v '\.ok$$')
export DEVICE

# TestFlight betas go out with the /beta skill, which reads these variables.
APP_ID     := 6816262826
IPA_PATH   := .beta-build/ios/Alondra.ipa
PKG_PATH   := .beta-build/macos/Alondra.pkg
BETA_GROUP := Internal Pilot,Private Pilot
APPLE_VERSION = $(shell sed -n 's/^ *version = "\(.*\)".*/\1/p' tools/shared.bzl)
APPLE_BUILD = $(shell sed -n 's/^ *build_number = "\(.*\)".*/\1/p' tools/shared.bzl)

.PHONY: version-guard increment-version build-release snapshots-record snapshots-verify tui-shots mac-snapshots-record mac-snapshots-verify no-holds build test install run tui setup launchd unlaunchd restart status doctor cargo-test mac device device-console e2e-sandbox e2e-sandbox-stop e2e-wizard e2e-doc-review e2e-sandbox-logs e2e-relay-cards e2e-direct-cards e2e-allow-always e2e-cards-tui e2e-ios-cards e2e-ios-offline soak load-bench tasks-screen rehearse-cutover upgrade-rehearsal cutover-live clean-xcode-test-results

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

TUI_SHOTS ?= /tmp/alondra-tui-shots
CHROME ?= /Applications/Google Chrome.app/Contents/MacOS/Google Chrome

# The PR card flow (merge method, merging, refused, changed) drawn by the TUI, in es and en.
tui-shots:
	bazelisk test //crates/alondra:alondra_test --test_arg=--ignored --test_arg=tui_shots \
		--nocache_test_results --zip_undeclared_test_outputs=false --test_output=errors
	@mkdir -p $(TUI_SHOTS)
	@for page in bazel-testlogs/crates/alondra/alondra_test/test.outputs/*.html; do \
		"$(CHROME)" --headless=new --disable-gpu --hide-scrollbars --window-size=1110,664 \
			--screenshot="$(TUI_SHOTS)/$$(basename $$page .html).png" "file://$(CURDIR)/$$page" >/dev/null 2>&1; \
	done
	@ls $(TUI_SHOTS)

# Result bundles `xcodebuild` left in DerivedData before the iOS test runner kept them in Bazel's
# own outputs: logs only, nothing builds from them.
clean-xcode-test-results:
	@before=$$(du -sk "$(HOME)/Library/Developer/Xcode/DerivedData" 2>/dev/null | cut -f1); \
	find "$(HOME)/Library/Developer/Xcode/DerivedData" -maxdepth 1 -type d -name 'temporary-*' -exec rm -rf {} +; \
	after=$$(du -sk "$(HOME)/Library/Developer/Xcode/DerivedData" 2>/dev/null | cut -f1); \
	echo "DerivedData: $$((before / 1024)) MB -> $$((after / 1024)) MB"

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
	install -m 755 $(call exe,//crates/alondrad) $(BIN)/alondrad
	install -m 755 $(call exe,//crates/alondra) $(BIN)/alondra
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
	ALONDRA_BIN=$(BIN) $(BIN)/alondra doctor --fix --no-input

no-holds:
	@holds=$$(curl -s --max-time 3 http://127.0.0.1:47800/v1/health | sed -n 's/.*"holds":\([0-9]*\).*/\1/p'); \
	if [ -n "$$holds" ] && [ "$$holds" != "0" ] && [ -z "$(FORCE)" ]; then \
		echo "alondrad is holding $$holds question(s) or permission(s) for nodes; restarting would cut them. Answer them first or run with FORCE=1."; exit 1; \
	fi

restart: no-holds
	launchctl kickstart -k gui/$$(id -u)/$(LABEL)

launchd: brew-guard no-holds install
	ALONDRA_BIN=$(BIN) $(BIN)/alondra doctor --fix --no-input --only service

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
	tools/e2e-sandbox.sh $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

e2e-sandbox-stop:
	tools/e2e-sandbox-stop.sh

# alondra's setup wizard on a new Mac in its own throwaway sandbox (tools/e2e-wizard.py).
e2e-wizard:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e-wizard.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# A document reviewed in the real TUI (inside tmux) against its own sandbox: the node gets the
# comments and the TUI says they were sent (tools/e2e-doc-review.py). E2E_CAPTURES=<dir> keeps the screens.
e2e-doc-review:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e-doc-review.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# Every kind of card answered from one client against its own sandbox: each leaves at once and never
# comes back, and each card of a group opens on its own screen (tools/e2e_cards.py, tests/sandbox/README.md).
# e2e-relay-cards and e2e-direct-cards use the synthetic phone; e2e-cards-tui the real TUI in tmux;
# e2e-ios-cards the iOS app in a pooled simulator. E2E_CAPTURES=<dir> keeps what each client showed.
E2E_CARDS = bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox && \
	python3 tools/e2e_cards.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) --client $(1) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

e2e-relay-cards:
	$(call E2E_CARDS,relay)

e2e-direct-cards:
	$(call E2E_CARDS,direct)

e2e-cards-tui:
	$(call E2E_CARDS,tui)

E2E_IOS_APP = //apple:AlondraiOS --ios_multi_cpus=sim_arm64 --define=alondra_sandbox=1

e2e-ios-cards:
	bazelisk build $(E2E_IOS_APP)
	export E2E_IOS_IPA=$$(bazelisk cquery --output=files $(E2E_IOS_APP) 2>/dev/null | grep '\.ipa$$'); $(call E2E_CARDS,ios)

# «Allow always» with the real Claude Code (HOME in a temporary folder, a local server as the model)
# against its own sandbox: the cases of docs/permitir-siempre-design.md §9.4 (tools/e2e_allow_always.py).
e2e-allow-always:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	python3 tools/e2e_allow_always.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# The iOS app loses its Mac with the app open (daemon frozen) and finds it again by itself, and at
# once back in the foreground (tools/e2e_ios_offline.py). E2E_CAPTURES=<dir> keeps what it showed.
e2e-ios-offline:
	bazelisk build $(E2E_IOS_APP) //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	export E2E_IOS_IPA=$$(bazelisk cquery --output=files $(E2E_IOS_APP) 2>/dev/null | grep '\.ipa$$'); \
	python3 tools/e2e_ios_offline.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# MAC=n for that daemon's log, MAC=relay for the relay's.
e2e-sandbox-logs:
	@dir=$${SANDBOX_DIR:-/tmp/alondra-sandbox}; \
	if [ "$(MAC)" = relay ]; then tail -f $$dir/relay.log; else tail -f $$dir/mac-$${MAC:-1}/alondrad.log; fi

# Full cutover rehearsal on a read-only snapshot of the live HOME; keeps only the report.
# REHEARSAL_ROOT, LIVE_HOME and LEGACY override the defaults (tools/rehearse-cutover.sh).
rehearse-cutover:
	bazelisk build //crates/alondrad
	tools/rehearse-cutover.sh $(call exe,//crates/alondrad)

# The previous published release must restart into this build on copies of the live HOME
# (deploy/brew/upgrade-rehearsal.py; the release runs it on the signed archive). Bazel stamps 0.1.0, so the build
# replaces the newest published release unless FROM=version names another.
upgrade-rehearsal:
	bazelisk build //crates/alondrad
	python3 deploy/brew/upgrade-rehearsal.py --new $(call exe,//crates/alondrad) --from $(or $(FROM),latest)

# The one-command cutover of the live HOME to domain v2 (docs/domain-v2-live-cutover.md).
# BACKUP_ROOT, LEGACY_DAEMON and LEGACY_CLI override the defaults (tools/cutover-live.sh).
cutover-live:
	bazelisk build //crates/alondrad
	tools/cutover-live.sh $(call exe,//crates/alondrad)

# Latency of /v1/health, views and hooks plus alondrad's CPU under N simulated nodes (tools/load_bench.py).
load-bench:
	bazelisk build //crates/alondrad //crates/alondra //crates/alondrad:alondrad_test
	python3 tools/load_bench.py --daemon bazel-bin/crates/alondrad/alondrad \
		--cli bazel-bin/crates/alondra/alondra --seed bazel-bin/crates/alondrad/alondrad_test $(LOAD_BENCH_ARGS)

soak:
	bazelisk build //crates/alondrad
	taskpolicy -b python3 tools/soak.py --binary $(call exe,//crates/alondrad) $(SOAK_ARGS)

# The TUI's Tasks tab for the running alondrad's overview, at 130x33, as text (SCREEN=path keeps it).
tasks-screen:
	curl -sf http://127.0.0.1:47800/v1/overview -o /tmp/alondra-overview.json
	ALONDRA_OVERVIEW=/tmp/alondra-overview.json ALONDRA_SCREEN=$(or $(SCREEN),/tmp/alondra-tasks.txt) cargo test -q -p alondra tasks_screen_of_an_overview -- --ignored
	cat $(or $(SCREEN),/tmp/alondra-tasks.txt)

# Fails when tools/shared.bzl is below a build App Store Connect already has, for either app.
version-guard:
	python3 deploy/apple/version_guard.py

increment-version:
	@set -e; \
	NEW_BUILD=$$(( $(APPLE_BUILD) + 1 )); \
	sed -i '' 's/build_number = "$(APPLE_BUILD)"/build_number = "'$$NEW_BUILD'"/' tools/shared.bzl; \
	python3 deploy/apple/version_guard.py || { git checkout -- tools/shared.bzl; exit 1; }; \
	git add tools/shared.bzl; \
	git commit -m "🔖 Release $(APPLE_VERSION) ($$NEW_BUILD)"; \
	git tag -a "v$(APPLE_VERSION)-$$NEW_BUILD" -m "Release $(APPLE_VERSION) build $$NEW_BUILD"

# Signed, verified IPA and PKG at IPA_PATH and PKG_PATH; uploads nothing.
build-release: version-guard
	python3 deploy/apple/prepare.py ios --output .beta-build/ios
	python3 deploy/apple/prepare.py macos --output .beta-build/macos
