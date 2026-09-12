.PHONY: build install uninstall test-topology dev web-bundle omarc check-elan

BINARIES := omar omar-computer omar-slack
INSTALL_DIR := $(HOME)/.cargo/bin
OMARC := lang/.lake/build/bin/omarc

# Mission Control, built and compressed for the runtime to embed. Needs Node.
web-bundle:
	cd web && npm ci && npm run build:spa

# Named up front, because the alternative is `lake: command not found` -- and
# for `install`, only after the release build has already run.
check-elan:
	@command -v lake >/dev/null 2>&1 || { \
	  printf '%s\n' \
	    "error: lake not found, so omarc cannot be built." \
	    "" \
	    "omarc is the OMAR compiler. Every .omar program is compiled by it," \
	    "so building from source needs elan, the Lean toolchain manager:" \
	    "" \
	    "  curl https://elan.lean-lang.org/elan-init.sh -sSf | sh" \
	    "" \
	    "See lang/README.md." >&2; \
	  exit 1; \
	}

# The OMAR compiler. Lean-built, so cargo cannot produce it; `omar run` shells
# out to it for every `.omar` program. Needs elan; see lang/README.md.
omarc: check-elan
	cd lang && lake build omarc

# Built with the UI in it, so an installed `omar serve --ui` has something to
# serve. A plain `cargo build --release` still works and still needs no Node;
# it just cannot serve the UI.
build: web-bundle
	cargo build --release --features ui

# omarc lands beside omar, which is the first place the runtime looks for it.
# Without it an installed omar falls back to the compiler in whatever source
# tree it was built from, and breaks when that tree moves.
install: check-elan build omarc
	install -d $(INSTALL_DIR)
	install $(addprefix target/release/,$(BINARIES)) $(INSTALL_DIR)/
	install $(OMARC) $(INSTALL_DIR)/

uninstall:
	rm -f $(addprefix $(INSTALL_DIR)/,$(BINARIES) omarc)

test-topology:
	OMAR_TEST_CASE="$(CASE)" ./tests/topology/run_local.sh

# The daemon and Mission Control together, pointed at each other. Ctrl-C stops
# both. Needs Node; see web/README.md. Deploying from the UI compiles a
# program, so the compiler has to be there before the daemon starts.
dev: omarc
	./web/dev.sh
