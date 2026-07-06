# AVR64EA28 bootloader: proven core + this port, two TWI address variants.
#
# Install (per board, Task 11 docs):
#   avrdude -c atmelice_updi -p avr64ea28 -U bootsize:w:8:m \
#           -U flash:w:build/firmware_0xNN.hex:i
#   (BOOTSIZE = 8 x 512 = 4 KiB boot section => APP_BASE 0x1000)
# App projects link with -Wl,--section-start=.text=0x1000; their vectors
# land at the app base because CPUINT.CTRLA IVSEL resets to 0 (vectors
# directly after the boot section, DS40002443A 15.5.1).

MCU     := avr64ea28
CC      := avr-gcc
OBJCOPY := avr-objcopy
OBJDUMP := avr-objdump
SIZE    := avr-size
BUILD   := build
CORE    := ../../core
INC     := ../../include
COMMON  := ../avr_ea_common

# -DNDEBUG: the core's runtime assert()s restate obligations already
# discharged by Frama-C/CBMC; avr-libc's assert would drag abort() into a
# 4 KiB bootloader for zero information.
CFLAGS := -mmcu=$(MCU) -Os -flto -std=c11 -Wall -Wextra -Werror -DNDEBUG \
          -I$(INC) -I. -I$(COMMON)

SRC := main.c twi.c $(COMMON)/flash.c $(COMMON)/entry.c \
       $(CORE)/update.c $(CORE)/proto.c $(CORE)/crc32.c $(CORE)/crc8.c

MAXSIZE := 4096

.PHONY: all clean
.SECONDARY:   # keep the .elf files: PORT_AUDIT.md checks are made on them
all: $(BUILD)/firmware_0x10.hex $(BUILD)/firmware_0x20.hex

# Size gate: the image must fit the 4 KiB boot section, or the part is
# unbootable. text+data is the flash footprint; the build FAILS over budget.
#
# .init3 self-check (PORT_AUDIT E2): updater_entry_capture runs before the
# stack exists (SP=RAMEND, and the re-entry pair lives AT RAMEND), so its
# disassembly must contain no push/pop/call/ret — any of those means the
# compiler grew a prologue or helper call and the pair is destroyed before
# capture. Checked on every build, not only at audit time; a missing symbol
# also fails (an LTO rename would otherwise blind the check).
$(BUILD)/firmware_%.elf: $(SRC) port_cfg.h $(COMMON)/port_geom.h $(wildcard $(INC)/updater/*.h) Makefile
	@mkdir -p $(BUILD)
	$(CC) $(CFLAGS) -DUPDATER_TWI_ADDR=$* $(SRC) -o $@
	@set -- $$($(SIZE) -B $@ | awk 'NR==2 {print $$1, $$2}'); \
	  used=$$(($$1 + $$2)); \
	  echo "$@: text+data = $$used / $(MAXSIZE) bytes"; \
	  if [ $$used -gt $(MAXSIZE) ]; then \
	    echo "SIZE GATE FAILED: exceeds boot section"; rm -f $@; exit 1; \
	  fi
	@$(OBJDUMP) -d $@ \
	  | awk '/^[0-9a-f]+ <updater_entry_capture[^>]*>:/{f=1;print;next} \
	         f && /^[0-9a-f]+ <\./{print;next} \
	         f && /^[0-9a-f]+ </{exit} f' \
	  > $@.init3.dis; \
	  if ! grep -q 'updater_entry_capture' $@.init3.dis; then \
	    echo "INIT3 CHECK FAILED: updater_entry_capture not in disassembly"; \
	    rm -f $@; exit 1; \
	  fi; \
	  if grep -Eq '\b(push|pop|call|rcall|icall|ret|reti)\b' $@.init3.dis; then \
	    echo "INIT3 CHECK FAILED: stack-using instruction in updater_entry_capture:"; \
	    grep -E '\b(push|pop|call|rcall|icall|ret|reti)\b' $@.init3.dis; \
	    rm -f $@; exit 1; \
	  fi; \
	  echo "$@: .init3 capture is push/call/ret-free"

$(BUILD)/firmware_%.hex: $(BUILD)/firmware_%.elf
	$(OBJCOPY) -O ihex -R .eeprom $< $@

clean:
	rm -rf $(BUILD)
