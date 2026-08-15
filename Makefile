# ccft - an agentic self improvement tool
#
# One artifact: the binary. No install dir, no rsync.
# All filesystem state under: ~/.config/ccft/, ~/.local/share/ccft/, ~/.cc-flytrap/

CARGO    ?= cargo
BIN_NAME ?= ccft
TARGET   ?= release

# Where the build lands.
BUILT_BIN := target/$(TARGET)/$(BIN_NAME)

# Where it gets installed.
INSTALL_DIR := $(HOME)/.local/bin
INSTALL_BIN := $(INSTALL_DIR)/$(BIN_NAME)

.PHONY: help check clippy build dev install uninstall start stop status restart trust logs clean test smoke

# Default target — fastest correctness signal during dev.
.DEFAULT_GOAL := check

help:
	@echo "ccft - an agentic self improvement tool"
	@echo ""
	@echo "fast iteration (no link):"
	@echo "  make check       cargo check (parse + types + borrow, ~5s incremental)"
	@echo "  make clippy      lints + check"
	@echo ""
	@echo "build + install:"
	@echo "  make build       compile a release binary at $(BUILT_BIN)"
	@echo "  make dev         set up + register the parallel dev service (com.ccft.dev)"
	@echo "  make install     build + ./ccft install"
	@echo "  make uninstall   ./ccft uninstall"
	@echo ""
	@echo "lifecycle:"
	@echo "  make start       ccft start    (kick launchd)"
	@echo "  make stop        ccft stop     (bootout)"
	@echo "  make restart     ccft restart"
	@echo "  make status      ccft status"
	@echo ""
	@echo "  make trust       print env vars for routing any coding agent"
	@echo "  make logs        tail launchd output"
	@echo ""
	@echo "  make test        cargo test"
	@echo "  make clean       cargo clean"

check:
	$(CARGO) check

clippy:
	$(CARGO) clippy -- -D warnings

build:
	$(CARGO) build --$(TARGET)

dev: build
	$(BUILT_BIN) dev

install: build
	$(BUILT_BIN) install

uninstall:
	@if [ -x "$(INSTALL_BIN)" ]; then \
		"$(INSTALL_BIN)" uninstall; \
	elif [ -x "$(BUILT_BIN)" ]; then \
		"$(BUILT_BIN)" uninstall; \
	else \
		echo "no ccft binary found — already uninstalled?"; \
	fi

start:
	"$(INSTALL_BIN)" start

stop:
	"$(INSTALL_BIN)" stop

status:
	@if [ -x "$(INSTALL_BIN)" ]; then "$(INSTALL_BIN)" status; \
	else "$(BUILT_BIN)" status; fi

restart:
	"$(INSTALL_BIN)" restart

trust:
	@if [ -x "$(INSTALL_BIN)" ]; then "$(INSTALL_BIN)" trust; \
	else "$(BUILT_BIN)" trust; fi

logs:
	@if [ -x "$(INSTALL_BIN)" ]; then "$(INSTALL_BIN)" logs; \
	else "$(BUILT_BIN)" logs; fi

test:
	$(CARGO) test

# Isolated install/uninstall smoke against /tmp/ccft-smoke. Never touches
# real ~/.local/bin, ~/Library/LaunchAgents, or the launchctl gui domain.
SMOKE_PREFIX := /tmp/ccft-smoke
smoke: build
	@rm -rf $(SMOKE_PREFIX)
	@echo "─── isolated install (CCFT_PREFIX=$(SMOKE_PREFIX)) ───"
	@CCFT_PREFIX=$(SMOKE_PREFIX) $(BUILT_BIN) install
	@echo
	@echo "─── status ───"
	@CCFT_PREFIX=$(SMOKE_PREFIX) $(BUILT_BIN) status
	@echo
	@echo "─── what's on disk under the prefix ───"
	@find $(SMOKE_PREFIX) -type f | sort
	@echo
	@echo "─── isolated uninstall ───"
	@CCFT_PREFIX=$(SMOKE_PREFIX) $(BUILT_BIN) uninstall
	@echo
	@echo "─── verify clean ───"
	@CCFT_PREFIX=$(SMOKE_PREFIX) $(BUILT_BIN) status
	@find $(SMOKE_PREFIX) -type f 2>/dev/null | sort || true
	@rm -rf $(SMOKE_PREFIX)
	@echo
	@echo "✓ smoke passed — production install untouched"

clean:
	$(CARGO) clean
