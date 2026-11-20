DAEMON := daemon
DEBUG := $(DAEMON)/.build/debug
RELEASE := $(DAEMON)/.build/release
BUNDLE := dist/Rewind.app

.PHONY: build release bundle daemon run status list doctor ui measure clean

build:
	cd $(DAEMON) && swift build

release:
	cd $(DAEMON) && swift build -c release

bundle: release
	@rm -rf $(BUNDLE)
	@mkdir -p $(BUNDLE)/Contents/MacOS
	@cp $(RELEASE)/rewindd $(BUNDLE)/Contents/MacOS/Rewind
	@cp resources/Info.plist $(BUNDLE)/Contents/Info.plist
	@codesign --force --sign - $(BUNDLE)
	@echo "built $(BUNDLE)"

daemon: build
	$(DEBUG)/rewindd

run: build
	$(DEBUG)/rewindd run

status: build
	$(DEBUG)/rewindctl status

list: build
	$(DEBUG)/rewindd list

doctor: build
	$(DEBUG)/rewindd doctor

ui:
	cd ui && cargo build

measure: build
	./scripts/measure.sh

clean:
	rm -rf $(DAEMON)/.build ui/target dist
