# Aether Gate 4 host spike: Wayland client bridge.
#
# Builds a drop-in libwayland-client.so.0 (client ABI, in-process fake
# compositor) and a libwayland-cursor.so.0 fallback.
#
#   make            # build into $(BUILD)
#   make clean

CC ?= cc
BUILD ?= /tmp/opencode/g4-shim-build
CFLAGS ?= -O2 -g -fPIC -fvisibility=hidden -Wall -Wextra
LDFLAGS ?= -shared

all: $(BUILD)/libwayland-client.so.0 $(BUILD)/libwayland-cursor.so.0

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/keymap.o: keymap.xkb | $(BUILD)
	cd $(BUILD) && cp $(CURDIR)/keymap.xkb . && \
		ld -r -b binary -o keymap.o keymap.xkb

$(BUILD)/wayland_shim.o: wayland_shim.c | $(BUILD)
	$(CC) $(CFLAGS) -c -o $@ $<

$(BUILD)/libwayland-client.so.0: $(BUILD)/wayland_shim.o $(BUILD)/keymap.o
	$(CC) $(LDFLAGS) -Wl,-soname,libwayland-client.so.0 -o $@ $^ -lpthread
	ln -sf libwayland-client.so.0 $(BUILD)/libwayland-client.so

$(BUILD)/libwayland-cursor.so.0: wayland_cursor_stub.c | $(BUILD)
	$(CC) $(CFLAGS) $(LDFLAGS) -Wl,-soname,libwayland-cursor.so.0 -o $@ $<
	ln -sf libwayland-cursor.so.0 $(BUILD)/libwayland-cursor.so

clean:
	rm -f $(BUILD)/libwayland-client.so.0 $(BUILD)/libwayland-client.so \
	      $(BUILD)/libwayland-cursor.so.0 $(BUILD)/libwayland-cursor.so \
	      $(BUILD)/wayland_shim.o $(BUILD)/keymap.o

.PHONY: all clean
