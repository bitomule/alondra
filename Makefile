BIN ?= $(HOME)/.local/bin
PLUGIN := $(CURDIR)/plugin
LABEL = $(shell $(BIN)/alondra config get service_label 2>/dev/null)
DEVICE ?=
# A Rust target's executable: cquery also lists the clippy report it builds beside it.
exe = $$(bazelisk cquery --output=files $(1) 2>/dev/null | grep -v '\.ok$$')
in_execroot = $$(bazelisk info execution_root 2>/dev/null)/$(call exe,$(1))
export DEVICE

# TestFlight betas go out with the /beta skill, which reads these variables. They are the published app's
# App Store Connect id and its TestFlight groups, only used to release; building and testing ignore them.
APP_ID     := 6816262826
IPA_PATH   := .beta-build/ios/Alondra.ipa
PKG_PATH   := .beta-build/macos/Alondra.pkg
BETA_GROUP := Internal Pilot,Private Pilot
APPLE_VERSION = $(shell sed -n 's/^ *version = "\(.*\)".*/\1/p' tools/shared.bzl)
APPLE_BUILD = $(shell sed -n 's/^ *build_number = "\(.*\)".*/\1/p' tools/shared.bzl)

.PHONY: action-outcomes connection-measurements musts-scopes reap-test-hosts version-guard increment-version build-release snapshots-record snapshots-verify tui-shots mac-snapshots-record mac-snapshots-verify mermaid-snapshots-record mermaid-fixtures-record no-holds build test install run tui setup launchd unlaunchd restart status doctor mac device device-console e2e-sandbox e2e-sandbox-stop tui-sandbox tui-sandbox-stop demo-captures e2e-iroh e2e-iroh-cli e2e-wizard e2e-doc-review e2e-doc-links e2e-pr-images e2e-doc-mermaid e2e-sandbox-logs e2e-direct-cards e2e-allow-always e2e-turn-end-wait e2e-interrupt-unheard e2e-cards-tui e2e-tui-optimistic e2e-ios-cards e2e-ios-live-activity e2e-plan-card e2e-card-attachments e2e-plan-result e2e-plan-review e2e-pause-queued e2e-cancel e2e-ios-offline soak load-bench views-measure read-index-bench tasks-screen upgrade-rehearsal clean-xcode-test-results e2e-journeys e2e-sdk e2e-explore skill-evals

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

# Only a screen that no longer matches its baseline (or has none) is recorded and reported FAILED;
# its image lands in the test outputs and is copied back. Every other baseline keeps its bytes.
snapshots-record:
	-bazelisk test $(SNAPSHOT_TARGET) \
		--test_env=SNAPSHOT_TESTING_RECORD=failed \
		--test_env=SNAPSHOT_ROOT=$(SNAPSHOT_ROOT) \
		--nocache_test_results --test_output=errors
	@copied=; for outputs in $$(find bazel-testlogs/apple/Testing/Snapshots -type d -name test.outputs); do \
		if [ -d "$$outputs/$(SNAPSHOT_ROOT)" ]; then \
			mkdir -p $(SNAPSHOT_ROOT) && cp -R "$$outputs/$(SNAPSHOT_ROOT)/." $(SNAPSHOT_ROOT)/ && chmod -R u+w $(SNAPSHOT_ROOT); \
			copied=yes; \
		fi; \
	done; \
	if [ -n "$$copied" ]; then \
		echo "Baselines copied to $(SNAPSHOT_ROOT)"; \
	else \
		echo "Every baseline still matches its screen: nothing recorded"; \
	fi

snapshots-verify:
	bazelisk test $(SNAPSHOT_TARGET) --test_output=errors

TUI_SHOTS ?= /tmp/alondra-tui-shots
CHROME ?= /Applications/Google Chrome.app/Contents/MacOS/Google Chrome

# The PR card flow (merge method, merging, refused, changed) and the usage sheet drawn by the TUI, in es and en.
tui-shots:
	bazelisk test //crates/alondra:alondra_test --test_arg=--ignored --test_arg=tui_shots --test_env=RUST_MIN_STACK=268435456 \
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
		--test_env=SNAPSHOT_TESTING_RECORD=failed \
		--test_env=SNAPSHOT_ROOT=$(MAC_SNAPSHOT_ROOT) \
		--nocache_test_results --test_output=errors
	@outputs=$$(find bazel-testlogs/apple/Testing/MacSnapshots/AlondraMacSnapshotTests -type d -name test.outputs | head -1); \
	if [ -n "$$outputs" ] && [ -d "$$outputs/$(MAC_SNAPSHOT_ROOT)" ]; then \
		mkdir -p $(MAC_SNAPSHOT_ROOT) && cp -R "$$outputs/$(MAC_SNAPSHOT_ROOT)/." $(MAC_SNAPSHOT_ROOT)/ && chmod -R u+w $(MAC_SNAPSHOT_ROOT); \
		echo "Baselines copied to $(MAC_SNAPSHOT_ROOT)"; \
	else \
		echo "Every baseline still matches its screen: nothing recorded"; \
	fi

MERMAID_SNAPSHOT_TARGET := //apple/Mermaid:AlondraMermaidSnapshotTests
MERMAID_SNAPSHOT_ROOT := apple/Mermaid/Snapshots/__Snapshots__

mermaid-snapshots-record:
	-bazelisk test $(MERMAID_SNAPSHOT_TARGET) \
		--test_env=SNAPSHOT_TESTING_RECORD=failed \
		--nocache_test_results --test_output=errors
	@outputs=$$(find bazel-testlogs/apple/Mermaid/AlondraMermaidSnapshotTests -type d -name test.outputs | head -1); \
	if [ -n "$$outputs" ] && [ -d "$$outputs/$(MERMAID_SNAPSHOT_ROOT)" ]; then \
		mkdir -p $(MERMAID_SNAPSHOT_ROOT) && cp -R "$$outputs/$(MERMAID_SNAPSHOT_ROOT)/." $(MERMAID_SNAPSHOT_ROOT)/ && chmod -R u+w $(MERMAID_SNAPSHOT_ROOT); \
		echo "Baselines copied to $(MERMAID_SNAPSHOT_ROOT)"; \
	else \
		echo "Every baseline still matches its screen: nothing recorded"; \
	fi

# Writes what each Mermaid fixture produces (`.tree`, `.error`, `.crossings`) into the test
# outputs and copies it over Fixtures/: review the diff before committing it.
mermaid-fixtures-record:
	-bazelisk test //apple/Mermaid:AlondraMermaidTests --test_env=MERMAID_FIXTURES_RECORD=1 \
		--zip_undeclared_test_outputs=false --nocache_test_results --test_output=errors
	@outputs=bazel-testlogs/apple/Mermaid/AlondraMermaidTests/test.outputs; \
	for file in $$outputs/*.tree $$outputs/*.crossings; do cp "$$file" apple/Mermaid/Fixtures/valid/; done; \
	for file in $$outputs/*.error; do cp "$$file" apple/Mermaid/Fixtures/invalid/; done; \
	chmod -R u+w apple/Mermaid/Fixtures

# The Mac app's connection measurements, summarized per path and network. An iPhone's come from
# Settings > Connection measurements > Share: `make connection-measurements MEASUREMENTS=<file>`.
MEASUREMENTS ?= $(HOME)/Library/Containers/com.bitomule.alondra.mac/Data/Library/Application Support/com.bitomule.alondra.mac/CommandJournal/connection-measurements.jsonl
connection-measurements:
	python3 tools/connection_measurements.py "$(MEASUREMENTS)" $(ARGS)

# How actions ended, per action and outcome, from the daemon's log: `make action-outcomes`, or
# `make action-outcomes ACTION_LOG=<file>` for another log.
ACTION_LOG ?= $(HOME)/.alondra/alondrad.log
action-outcomes:
	python3 tools/action_outcomes.py "$(ACTION_LOG)"

mac-snapshots-verify:
	bazelisk test $(MAC_SNAPSHOT_TARGET) --test_output=errors

brew-guard:
	@if [ -z "$(FORCE_DEV_INSTALL)" ] && { [ -e /opt/homebrew/opt/alondra ] || [ -e /usr/local/opt/alondra ]; }; then \
		echo "Alondra is installed with Homebrew; dev installs would replace the user's daemon. Use make e2e-sandbox, or FORCE_DEV_INSTALL=1 if you really mean it"; exit 1; \
	fi

install: brew-guard build
	mkdir -p $(BIN) $(PLUGIN)/bin $(HOME)/.alondra
	install -m 755 $(call exe,//crates/alondrad) $(BIN)/alondrad
	install -m 755 $(call exe,//crates/alondra) $(BIN)/alondra
	install -m 755 $(call exe,//crates/alondrad:alondrad-launcher) $(BIN)/alondrad-launcher
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
# Every Mac runs its tasks through the Agent SDK host with the sandbox's scripted Claude; SDK=1
# FAKES=1 gives Mac 1 the pinned native Claude instead (tools/e2e_sdk_backend.py).
SDK_HOST_TARGETS = //agent-host:pinned_node //agent-host:persistent.cjs
SDK_HOST_ENV = SDK_NODE=$(call in_execroot,//agent-host:pinned_node) SDK_HOST=$(call in_execroot,//agent-host:persistent.cjs)
e2e-sandbox:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS) \
		$(if $(filter 1,$(SDK)),@claude_native_test//:archive)
	$(SDK_HOST_ENV) $(if $(filter 1,$(SDK)),SDK_ARCHIVE=$(call in_execroot,@claude_native_test//:archive)) \
	tools/e2e-sandbox.sh $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

e2e-sandbox-stop:
	tools/e2e-sandbox-stop.sh

# Mac 1's TUI in the tmux session SESSION (alondra-tui) against a fresh seeded sandbox with the
# fakes, never the real daemon: how a change the user sees is reviewed (tools/tui-sandbox.sh).
tui-sandbox:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) tools/tui-sandbox.sh $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

tui-sandbox-stop:
	tools/tui-sandbox.sh --stop

# Real captures of the TUI, the iPhone app and the Mac app against a sandbox seeded with invented
# projects (tools/demo_captures.py): English and Spanish, light and dark, in DEMO_CAPTURES.
# ONLY=tui,ios,mac picks the clients.
DEMO_APP_FLAGS = --define=alondra_sandbox=1
DEMO_CLIENTS = tui,ios,mac
demo-captures:
	$(if $(findstring ios,$(or $(ONLY),ios)),bazelisk build //apple:AlondraiOS --ios_multi_cpus=sim_arm64 $(DEMO_APP_FLAGS))
	$(if $(findstring mac,$(or $(ONLY),mac)),bazelisk build //apple:AlondraMac $(DEMO_APP_FLAGS))
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) python3 tools/demo_captures.py --out $(or $(DEMO_CAPTURES),$(CURDIR)/.demo-captures) --only $(or $(ONLY),$(DEMO_CLIENTS)) \
		$(if $(findstring ios,$(or $(ONLY),ios)),--ios-ipa $$(bazelisk cquery --output=files //apple:AlondraiOS --ios_multi_cpus=sim_arm64 $(DEMO_APP_FLAGS) 2>/dev/null | grep '\.ipa$$')) \
		$(if $(findstring mac,$(or $(ONLY),mac)),--mac-zip $$(bazelisk cquery --output=files //apple:AlondraMac $(DEMO_APP_FLAGS) 2>/dev/null | grep '\.zip$$' | head -1)) \
		$(call exe,//crates/alondrad) $(call exe,//crates/alondra) $(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# Mac 1's iroh endpoint in a fresh throwaway sandbox: the same envelope gets the same answer over
# iroh as over the direct listener, a device never paired is refused, and both are measured.
e2e-iroh:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e-iroh.sh $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# The real alondra over iroh against a one-Mac sandbox and a real iroh-relay in Docker (the hosted
# relay's image): directly, then through the iroh relay only, then revoked (tools/e2e-iroh-cli.sh).
e2e-iroh-cli:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e-iroh-cli.sh $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# alondra's setup wizard on a new Mac in its own throwaway sandbox (tools/e2e-wizard.py).
e2e-wizard:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e-wizard.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# A document reviewed in the real TUI (inside tmux) against its own sandbox: the node gets the
# comments and the TUI says they were sent (tools/e2e-doc-review.py). E2E_CAPTURES=<dir> keeps the screens.
e2e-doc-review:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) tools/e2e-doc-review.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# A README-like document in the real TUI (tmux): its HTML reads as text and images, and v opens
# each block's links (tools/e2e-doc-links.py). E2E_CAPTURES=<dir> keeps the screens.
e2e-doc-links:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) tools/e2e-doc-links.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# A PR description's images in the real TUI: the daemon brings them from the sandbox's GitHub, a
# missing one refuses the presentation, `v` opens a copy (tools/e2e-pr-images.py). E2E_CAPTURES=<dir> keeps the screens.
e2e-pr-images:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) tools/e2e-pr-images.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# A document with Mermaid diagrams in the real TUI (tmux) at 80 and 120 columns: the four kinds draw,
# the rest show their code with a notice (tools/e2e-doc-mermaid.py). E2E_CAPTURES=<dir> keeps the screens.
e2e-doc-mermaid:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) tools/e2e-doc-mermaid.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# Every kind of card answered from one client against its own sandbox: each leaves at once and never
# comes back, and each card of a group opens on its own screen (tools/e2e_cards.py, tests/sandbox/README.md).
# e2e-direct-cards uses the synthetic phone; e2e-cards-tui the real TUI in tmux;
# e2e-ios-cards the iOS app in a pooled simulator. E2E_CAPTURES=<dir> keeps what each client showed.
E2E_CARDS = bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS) && \
	$(SDK_HOST_ENV) python3 tools/e2e_cards.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) --client $(1) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

e2e-direct-cards:
	$(call E2E_CARDS,direct)

e2e-cards-tui:
	$(call E2E_CARDS,tui)

# A cancelled task reads «Cancelando» until Alondra records the end of its node, then «Cancelada»,
# seen in the real TUI in tmux (tools/e2e_cancel.py). E2E_CAPTURES=<dir> keeps each screen.
e2e-cancel:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e_cancel.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# A decision of the user holds the tasks after it until the orchestrator reviews their plans, seen in
# the real TUI in tmux (tools/e2e_plan_review.py). E2E_CAPTURES=<dir> keeps each screen.
e2e-plan-review:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e_plan_review.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# Pausing a task that waits for its automatic start takes it out of the queue, seen in the real TUI in
# tmux (tools/e2e_pause_queued.py). E2E_CAPTURES=<dir> keeps each screen.
e2e-pause-queued:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	tools/e2e_pause_queued.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# The TUI's optimistic answers in the real TUI inside tmux, against its own sandbox: a reply leaves and
# its delivery comes back once its node stops (waits for the Mac), and an answer to a frozen alondrad comes back after
# 30 s with a notice (tools/e2e_tui_optimistic.py). Screens land in E2E_CAPTURES (.e2e-captures by
# default); PNG=1 also renders them as images.
e2e-tui-optimistic:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	python3 tools/e2e_tui_optimistic.py --captures $(or $(E2E_CAPTURES),$(CURDIR)/.e2e-captures) $(if $(PNG),--png) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

E2E_IOS_APP = //apple:AlondraiOS --ios_multi_cpus=sim_arm64 --define=alondra_sandbox=1

# A plan that cites a Markdown file shows the file inside, against its own sandbox
# (tools/e2e_plan_card.py): CLIENT=tui|ios|mac, captures in E2E_CAPTURES (.e2e-captures by default).
e2e-plan-card:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(if $(filter ios,$(CLIENT)),$(E2E_IOS_APP))
	$(if $(filter ios,$(CLIENT)),export E2E_IOS_IPA=$$(bazelisk cquery --output=files $(E2E_IOS_APP) 2>/dev/null | grep '\.ipa$$');) \
	python3 tools/e2e_plan_card.py --client $(or $(CLIENT),tui) --captures $(or $(E2E_CAPTURES),$(CURDIR)/.e2e-captures) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# A card with an image, a video and a text file (tools/e2e_card_attachments.py): two synthetic phones download
# them directly and over the relay; CLIENT=ios draws them in the iOS app, CLIENT=keep leaves the sandbox up for the Mac app.
e2e-card-attachments:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(if $(filter ios,$(CLIENT)),$(E2E_IOS_APP))
	$(if $(filter ios,$(CLIENT)),export E2E_IOS_IPA=$$(bazelisk cquery --output=files $(E2E_IOS_APP) 2>/dev/null | grep '\.ipa$$');) \
	python3 tools/e2e_card_attachments.py --client $(or $(CLIENT),phone) --captures $(or $(E2E_CAPTURES),$(CURDIR)/.e2e-captures) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# A research that ends in a plan card, and one that ends in a report alone (tools/e2e_plan_result.py):
# the report outlives its temporary folder, only approving the plan hands it on, and a report is only seen. CLIENT=tui|phone, captures in E2E_CAPTURES.
e2e-plan-result:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	python3 tools/e2e_plan_result.py --client $(or $(CLIENT),tui) --captures $(or $(E2E_CAPTURES),$(CURDIR)/.e2e-captures) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

e2e-ios-cards:
	bazelisk build $(E2E_IOS_APP)
	export E2E_IOS_IPA=$$(bazelisk cquery --output=files $(E2E_IOS_APP) 2>/dev/null | grep '\.ipa$$'); $(call E2E_CARDS,ios)

# «Allow always» on the permission cards of the sandbox's scripted nodes, through the Agent SDK host,
# against its own sandbox: the allow-always cases (tools/e2e_allow_always.py).
e2e-allow-always:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) python3 tools/e2e_allow_always.py $(call exe,//crates/alondrad) $(call exe,//crates/alondra) \
		$(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# A turn that ends on a message to the orchestrator reads «Esperando al orquestador» in the real TUI
# in tmux, with no card for the user (tools/e2e_turn_end_wait.py). E2E_CAPTURES=<dir> keeps each screen.
e2e-turn-end-wait:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) python3 tools/e2e_turn_end_wait.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) $(if $(TO_USER),--to-user) $(call exe,//crates/alondrad) \
		$(call exe,//crates/alondra) $(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# A task whose background command ended without waking Claude offers Interrupt and Pause in the
# real TUI (tmux), and Interrupt hands Claude the message it held (tools/e2e_interrupt_unheard.py).
e2e-interrupt-unheard:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) python3 tools/e2e_interrupt_unheard.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) $(call exe,//crates/alondrad) \
		$(call exe,//crates/alondra) $(call exe,//crates/relay:alondra-relay) $(call exe,//tools/sandbox:alondra-sandbox)

# The iOS app loses its Mac with the app open (daemon frozen) and finds it again by itself, and at
# once back in the foreground (tools/e2e_ios_offline.py). E2E_CAPTURES=<dir> keeps what it showed.
e2e-ios-offline:
	bazelisk build $(E2E_IOS_APP) //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	export E2E_IOS_IPA=$$(bazelisk cquery --output=files $(E2E_IOS_APP) 2>/dev/null | grep '\.ipa$$'); \
	python3 tools/e2e_ios_offline.py $(if $(E2E_CAPTURES),--captures $(E2E_CAPTURES)) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# The real Live Activity in a pooled simulator (tools/e2e_ios_live_activity.py): the Lock Screen,
# light and dark, English and Spanish, in E2E_CAPTURES (.e2e-captures by default).
e2e-ios-live-activity:
	bazelisk build $(E2E_IOS_APP) //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox
	export E2E_IOS_IPA=$$(bazelisk cquery --output=files $(E2E_IOS_APP) 2>/dev/null | grep '\.ipa$$'); \
	python3 tools/e2e_ios_live_activity.py --captures $(or $(E2E_CAPTURES),$(CURDIR)/.e2e-captures) \
		$$(bazelisk cquery --output=files '//crates/alondrad + //crates/alondra + //crates/relay:alondra-relay + //tools/sandbox:alondra-sandbox' 2>/dev/null | grep -v '\.ok$$')

# Scripted journeys on a fresh sandbox with the fake claude and gh (tools/sandbox/src/journey);
# JOURNEY=task-cycle|session|reopen|mcp|report|local|accounts runs one. Evidence: bazel-testlogs/tests/sandbox/journeys/test.outputs.
# How the node and orchestrator skills behave with a real Claude run (plugin/evals,
# plugin-orchestrator/evals): opt-in, it costs tokens. RUNS=1 for a quick look.
skill-evals:
	tools/skill-evals.sh $(if $(RUNS),--runs $(RUNS))


# The SDK journey: a task through the Agent SDK host and the pinned native Claude, its turn-end card
# answered from the phone (tests/sandbox/sdk_journey.sh). Opt-in: macOS arm64, ~250 MB native binary.
e2e-sdk:
	bazelisk test //tests/sandbox:sdk_journey --nocache_test_results --zip_undeclared_test_outputs=false --test_output=streamed

e2e-journeys:
	bazelisk test //tests/sandbox:journeys --nocache_test_results --zip_undeclared_test_outputs=false \
		--test_output=streamed $(if $(JOURNEY),--test_env=JOURNEY=$(JOURNEY))

# The tester agent drives each Mac's TUI in tmux and writes a report (tools/e2e-explore.sh):
# MODEL (opus), BUDGET (USD, 5), FOCUS (extra instructions). Prints where the report is.
e2e-explore:
	bazelisk build //crates/alondrad //crates/alondra //crates/relay:alondra-relay //tools/sandbox:alondra-sandbox $(SDK_HOST_TARGETS)
	$(SDK_HOST_ENV) tools/e2e-explore.sh $$(bazelisk cquery --output=files //crates/alondrad 2>/dev/null) $$(bazelisk cquery --output=files //crates/alondra 2>/dev/null) \
		$$(bazelisk cquery --output=files //crates/relay:alondra-relay 2>/dev/null) $$(bazelisk cquery --output=files //tools/sandbox:alondra-sandbox 2>/dev/null)

# MAC=n for that daemon's log, MAC=relay for the relay's.
e2e-sandbox-logs:
	@dir=$${SANDBOX_DIR:-$$(git rev-parse --show-toplevel)/.scratch/sandbox}; \
	if [ "$(MAC)" = relay ]; then tail -F $$dir/relay.log; else tail -F $$dir/mac-$${MAC:-1}/alondrad.log; fi

# The previous published release must restart into this build on copies of the live HOME, and its
# claude --bg nodes move to the Agent SDK with their sessions, also with the daemon killed after each
# step (deploy/brew/upgrade-rehearsal.py; the release runs it on the signed archive). FROM=version
# names the release to upgrade from; by default the newest published one.
REHEARSAL_TARGETS = //crates/alondrad //crates/alondrad:alondrad-launcher //crates/alondra //tools/sandbox:alondra-sandbox \
	//agent-host:release_host
upgrade-rehearsal:
	bazelisk build $(REHEARSAL_TARGETS)
	python3 deploy/brew/upgrade-rehearsal.py --new $(call exe,//crates/alondrad) --cli $(call exe,//crates/alondra) \
		--host $$(bazelisk cquery --output=files //agent-host:release_host 2>/dev/null | grep 'alondra-host$$') \
		--sdk-fixtures agent-host --sandbox-tool $(call exe,//tools/sandbox:alondra-sandbox) --from $(or $(FROM),latest)

# Where each view's time and CPU go on a copy of a real domain:
# sqlite3 -readonly ~/.alondra/domain.db ".backup /path/copy.db" && make views-measure DB=/path/copy.db
views-measure:
	@test -n "$(DB)" || { echo "DB=<copy of domain.db> is required"; exit 1; }
	bazelisk build //crates/alondrad:alondrad_test
	home=$$(mktemp -d) && trap 'rm -rf "$$home"' EXIT && HOME="$$home" ALONDRA_VIEWS_MEASURE_DB="$(abspath $(DB))" \
		$(call exe,//crates/alondrad:alondrad_test) --ignored --exact views::measure_tests::measure_real_domain_views --nocapture

# Building the read index of a domain the size of the real one; fails over 10 ms.
read-index-bench:
	bazelisk run -c opt //crates/core:read_index_cost -- --check-cost

# Latency of /v1/health, views and hooks plus alondrad's CPU under N simulated nodes (tools/load_bench.py).
load-bench:
	bazelisk build //crates/alondrad //crates/alondra //crates/daemon-engine:daemon_engine_test
	python3 tools/load_bench.py --daemon bazel-bin/crates/alondrad/alondrad \
		--cli bazel-bin/crates/alondra/alondra --seed bazel-bin/crates/daemon-engine/daemon_engine_test $(LOAD_BENCH_ARGS)

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

# Fails when a bazel/test check's `paths` in MUSTS.yml miss a file its targets read (tools/musts_scopes.py).
# Ends the Alondra hosts that cut tests left running. DRY_RUN=1 lists them and ends nothing.
reap-test-hosts:
	python3 tools/reap-test-hosts.py $(if $(DRY_RUN),--dry-run)

musts-scopes:
	python3 tools/musts_scopes.py check $(CHECKS)
