#ifndef UPDATER_PROTO_H
#define UPDATER_PROTO_H
#include <stdbool.h>
#include <stdint.h>

#define UPD_PROTO_VERSION   1u

#define UPD_CMD_INFO        0x01u
#define UPD_CMD_ERASE_APP   0x02u
#define UPD_CMD_WRITE_PAGE  0x03u
#define UPD_CMD_VERIFY      0x04u
#define UPD_CMD_BOOT        0x05u
#define UPD_CMD_ECHO        0x06u
#define UPD_RSP_FLAG        0x80u

#define UPD_ST_OK           0x00u
#define UPD_ST_BAD_FRAME    0x01u
#define UPD_ST_BAD_CMD      0x02u
#define UPD_ST_NOT_ERASED   0x03u
#define UPD_ST_OUT_OF_RANGE 0x04u
#define UPD_ST_BAD_CRC      0x05u
#define UPD_ST_BAD_MAGIC    0x06u
#define UPD_ST_NO_APP       0x07u

#define UPD_ECHO_MAX        16u
/* Largest response payload upd_handle can produce: ECHO's ST byte + up to
 * UPD_ECHO_MAX echoed bytes (INFO's 12 is smaller). Size response-payload
 * buffers from this, not by re-deriving 17. */
#define UPD_RSP_MAX         (1u + UPD_ECHO_MAX)
/* frame = CMD + LEN + payload + CRC8; payload cap is set by the port's
 * page_size (WRITE_PAGE carries page_size + 2). Buffers are sized by the
 * port; the codec only checks internal consistency. */
#define UPD_FRAME_OVERHEAD  3u
/* Protocol-wide codec ceilings: LEN is a u8 and a whole frame fits 255
 * bytes, so a payload holds at most 255 - UPD_FRAME_OVERHEAD = 252 bytes.
 * These bound the CODEC; a port sizes its RX buffer from its geometry
 * (page_size + 8 accepted LEN — see port.h), never from these. */
#define UPD_LEN_MAX         252u
#define UPD_FRAME_MAX       255u

typedef struct {
    uint8_t        cmd;
    uint8_t        len;
    const uint8_t *payload;   /* points into the caller's receive buffer */
} upd_frame_t;

bool    upd_frame_parse(const uint8_t *buf, uint8_t buflen, upd_frame_t *out);
uint8_t upd_frame_build(uint8_t *buf, uint8_t cap, uint8_t cmd,
                        const uint8_t *payload, uint8_t len);

#endif
