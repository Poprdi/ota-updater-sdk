/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (c) 2026 Adrian Erlacher */

#ifndef CHECK_H
#define CHECK_H

#include <stdio.h>

static int g_failures;

#define CHECK(cond)                                                     \
    do {                                                                \
        if (!(cond)) {                                                  \
            printf("FAIL %s:%d  %s\n", __FILE__, __LINE__, #cond);      \
            g_failures++;                                               \
        }                                                               \
    } while (0)

#define TEST_RESULT(name)                                               \
    do {                                                                \
        if (g_failures) {                                               \
            printf("%s: %d check(s) failed\n", (name), g_failures);     \
            return 1;                                                   \
        }                                                               \
        printf("%s: all passed\n", (name));                            \
        return 0;                                                       \
    } while (0)

#endif
