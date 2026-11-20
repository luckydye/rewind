DAEMON := daemon
DEBUG := $(DAEMON)/.build/debug
RELEASE := $(DAEMON)/.build/release
BUNDLE := dist/Rewind.app

.PHONY: build ui ui-release daemon run bundle install uninstall status list doctor measure clean

# The daemon launches rewind-ui from its own directory, so the two binaries
# have to sit next to each other in development too.
build: ui
	cd $(DAEMON) && swift build
	@# Remove first: copying over an existing executable keeps its inode, and the
	@# kernel then kills the new binary on a stale code-signature cache (exit 137).
	@rm -f $(DEBUG)/rewind-ui
	@cp ui/target/debug/rewind-ui $(DEBUG)/rewind-ui

ui:
	cd ui && cargo build

ui-release:
	cd ui && cargo build --release

daemon: build

run: build
	$(DEBUG)/rewindd run

status: build
	$(DEBUG)/rewindctl status

list: build
	$(DEBUG)/rewindd list

doctor: build
	$(DEBUG)/rewindd doctor

bundle: ui-release
	cd $(DAEMON) && swift build -c release
	@rm -rf $(BUNDLE)
	@mkdir -p $(BUNDLE)/Contents/MacOS
	@cp $(RELEASE)/rewindd $(BUNDLE)/Contents/MacOS/Rewind
	@cp ui/target/release/rewind-ui $(BUNDLE)/Contents/MacOS/rewind-ui
	@cp resources/Info.plist $(BUNDLE)/Contents/Info.plist
	@codesign --force --sign - $(BUNDLE)
	@echo "built $(BUNDLE)"

APP := /Applications/Rewind.app

# Installs the built bundle as a login item and starts it. The daemon is the
# service; the overlay is preloaded by it.
install: bundle
	@pkill -x Rewind 2>/dev/null || true
	@pkill -x rewindd 2>/dev/null || true
	@pkill -x rewind-ui 2>/dev/null || true
	@sleep 1
	@rm -rf "$(APP)"
	@cp -R $(BUNDLE) "$(APP)" || { echo "cannot write $(APP) — retry with: sudo make install"; exit 1; }
	@echo "installed $(APP)"
	@"$(APP)/Contents/MacOS/Rewind" login-item on || echo "could not register the login item; toggle 'Start at Login' in the menu bar"
	@open "$(APP)"
	@echo "Rewind is running. macOS will ask for Screen Recording once — allow it, then reopen the overlay."

uninstall:
	@"$(APP)/Contents/MacOS/Rewind" login-item off 2>/dev/null || true
	@pkill -x Rewind 2>/dev/null || true
	@pkill -x rewind-ui 2>/dev/null || true
	@rm -rf "$(APP)"
	@echo "removed $(APP)"

measure: build
	./scripts/measure.sh

clean:
	rm -rf $(DAEMON)/.build ui/target dist
