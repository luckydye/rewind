DAEMON := daemon
BIN := $(DAEMON)/.build/debug/rewindd

.PHONY: build release run list doctor measure clean

build:
	cd $(DAEMON) && swift build

release:
	cd $(DAEMON) && swift build -c release

run: build
	$(BIN) run

list: build
	$(BIN) list

doctor: build
	$(BIN) doctor

measure: build
	./scripts/measure.sh

clean:
	rm -rf $(DAEMON)/.build
