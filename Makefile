DAEMON := daemon
DEBUG := $(DAEMON)/.build/debug
RELEASE := $(DAEMON)/.build/release
BUNDLE := dist/Rewind.app

.PHONY: build ui ui-release daemon run bundle status list doctor measure clean

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

measure: build
	./scripts/measure.sh

clean:
	rm -rf $(DAEMON)/.build ui/target dist
