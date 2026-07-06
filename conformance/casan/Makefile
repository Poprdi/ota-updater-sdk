# Sanitizer campaign runner: the sim + the real device core under
# ASan/UBSan, no recovery — any finding aborts and fails `make run`.
CC     := cc
SAN    := -fsanitize=address,undefined -fno-sanitize-recover=all
CFLAGS := -std=c11 -O1 -g -Wall -Wextra -Werror -pedantic -Wconversion \
          -I../../device/include -I../sim $(SAN)
CORE   := ../../device/core/crc8.c ../../device/core/crc32.c \
          ../../device/core/proto.c ../../device/core/update.c \
          ../../device/core/link_stream.c
SRC    := main.c ../sim/sim_port.c $(CORE)
BUILD  := build

.PHONY: all run clean
all: $(BUILD)/casan

HDRS   := ../sim/sim_port.h $(wildcard ../../device/include/updater/*.h)

$(BUILD)/casan: $(SRC) $(HDRS)
	@mkdir -p $(BUILD)
	$(CC) $(CFLAGS) $(SRC) -o $@

run: $(BUILD)/casan
	./$(BUILD)/casan

clean:
	rm -rf $(BUILD)
