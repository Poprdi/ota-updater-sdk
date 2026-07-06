# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 Adrian Erlacher
#
# RP2350 (Pico 2 W) bootloader: proven core + UART0 port (link_stream
# framing) — the cmake/ninja lane, deliberately separate from the AVR make
# lane and from the repo's host cargo lanes.
#
# Build (pico-sdk 2.1.1 cloned user-local, PICO_SDK_PATH set):
#   cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=MinSizeRel
#   ninja -C build
# Outputs:
#   build/bootloader.uf2   -> drop on the BOOTSEL drive (zero-tool install)
#                             or `picotool load -x bootloader.uf2`
#                             (install.toml target rp2350-bootsel)
#   build/blink.bin        -> minimal app image for the app region; flash it
#                             THROUGH the bootloader:
#                             updater-cli --transport uart flash blink.bin
#
# Baud is a build parameter: -DUPDATER_UART_BAUD=57600 (default 115200).
#
# Size gate: memmap_bootloader.ld pins FLASH to 64 KiB at the flash base —
# the LINK fails if the bootloader outgrows its reserved region. The app
# links with memmap_app.ld (app region: XIP base + 64 KiB, 1 MiB - 16).
cmake_minimum_required(VERSION 3.13...3.27)

# Board/platform must be set before the SDK import. pico2_w selects the
# RP2350A board definition; rp2350-arm-s is the Arm-S (secure Cortex-M33)
# platform the SDK builds IMAGE_DEFs for.
set(PICO_BOARD pico2_w CACHE STRING "Board type")
set(PICO_PLATFORM rp2350-arm-s CACHE STRING "Platform")

# Copied verbatim from pico-sdk 2.1.1 external/pico_sdk_import.cmake.
include(pico_sdk_import.cmake)

# C and ASM only: this port contains no C++, and this keeps the build
# working on toolchains without ARM libstdc++ (Debian ships it separately
# from gcc-arm-none-eabi). The SDK's single C++ translation unit — the
# operator new/delete stubs in pico_cxx_options, INTERFACE sources pulled
# into every executable via pico_runtime — is stripped below for the same
# reason: it serves only C++ applications and needs <cstdlib>/<new>.
project(updater_rp2350_uart C ASM)
pico_sdk_init()
set_property(TARGET pico_cxx_options PROPERTY INTERFACE_SOURCES "")

set(UPDATER_UART_BAUD 115200 CACHE STRING "Bootloader UART0 baud rate")

set(UPDATER_DEVICE ${CMAKE_CURRENT_LIST_DIR}/../..)
set(UPDATER_CORE   ${UPDATER_DEVICE}/core)
set(UPDATER_SKEL   ${UPDATER_DEVICE}/ports/skeletons)

# ---- bootloader ------------------------------------------------------------
set(BOOTLOADER_PORT_SRC
    ${CMAKE_CURRENT_LIST_DIR}/main.c
    ${CMAKE_CURRENT_LIST_DIR}/uart.c
    ${CMAKE_CURRENT_LIST_DIR}/flash.c
    ${CMAKE_CURRENT_LIST_DIR}/entry.c
    ${UPDATER_SKEL}/uart_pump.c
    ${UPDATER_CORE}/link_stream.c
    ${UPDATER_CORE}/update.c
    ${UPDATER_CORE}/proto.c
    ${UPDATER_CORE}/crc32.c
    ${UPDATER_CORE}/crc8.c
)
add_executable(bootloader ${BOOTLOADER_PORT_SRC})
target_include_directories(bootloader PRIVATE
    ${CMAKE_CURRENT_LIST_DIR}
    ${UPDATER_DEVICE}/include
    ${UPDATER_SKEL}
)
# NDEBUG: the core's runtime assert()s restate obligations already
# discharged by Frama-C/CBMC (same rationale as the AVR Makefiles).
#
# PICO_TIME_DEFAULT_ALARM_POOL_DISABLED: pico_stdlib -> pico_time would
# otherwise register the default alarm pool at runtime init
# (runtime_init_default_alarm_pool, sdk:src/common/pico_time/time.c),
# which claims a hardware alarm and ENABLES its NVIC IRQ with a
# flash-resident handler — inside a bootloader that only reads the
# timebase and must run IRQ-free (PORT_AUDIT U8/U9, F4). Bootloader
# target only: blink (and any real app) keeps the pool for sleep_ms.
target_compile_definitions(bootloader PRIVATE
    NDEBUG
    UPDATER_UART_BAUD=${UPDATER_UART_BAUD}
    PICO_TIME_DEFAULT_ALARM_POOL_DISABLED=1
)
# Warning police on OUR sources only: pico-sdk compiles its own sources
# into this target (INTERFACE sources), and -Werror on those is not our
# audit's business.
set_source_files_properties(${BOOTLOADER_PORT_SRC} PROPERTIES
    COMPILE_OPTIONS "-Wall;-Wextra;-Werror"
)
# pico_stdlib without stdio: no USB, no stdio-UART — UART0 is the port's
# transport, owned by uart.c alone. (pico_stdlib pulls runtime/clocks/
# time; stdio stays disabled below.)
target_link_libraries(bootloader
    pico_stdlib
    hardware_flash
    hardware_watchdog
)
pico_enable_stdio_usb(bootloader 0)
pico_enable_stdio_uart(bootloader 0)
pico_set_linker_script(bootloader ${CMAKE_CURRENT_LIST_DIR}/memmap_bootloader.ld)
pico_add_extra_outputs(bootloader)   # .uf2 for BOOTSEL install

# ---- minimal validation app (flashed THROUGH the bootloader) ---------------
add_executable(blink ${CMAKE_CURRENT_LIST_DIR}/app/blink.c)
target_include_directories(blink PRIVATE ${UPDATER_DEVICE}/include)
set_source_files_properties(${CMAKE_CURRENT_LIST_DIR}/app/blink.c PROPERTIES
    COMPILE_OPTIONS "-Wall;-Wextra;-Werror"
)
target_link_libraries(blink pico_stdlib hardware_watchdog)
pico_enable_stdio_usb(blink 0)
pico_enable_stdio_uart(blink 0)
pico_set_linker_script(blink ${CMAKE_CURRENT_LIST_DIR}/memmap_app.ld)
pico_add_extra_outputs(blink)        # blink.bin is the flashable image
