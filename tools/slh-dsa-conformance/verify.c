/*
 * Independent SP 800-230 IPD SLH-DSA-SHA2-128-24 verification harness.
 *
 * This file is compiled against pq-code-package/slhdsa-c at the commit pinned
 * by run.sh. It is deliberately outside the server workspace build.
 */

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "slh_dsa.h"
#include "slh_param.h"

#define MESSAGE_BYTES 32
#define PUBLIC_KEY_COMPONENT_BYTES 16
#define MAX_SIGNATURE_BYTES 8192

static int hex_nibble(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

static const char *skip_hex_prefix(const char *hex)
{
    if (hex[0] == '0' && (hex[1] == 'x' || hex[1] == 'X')) return hex + 2;
    return hex;
}

static int decode_exact(uint8_t *out, size_t out_len, const char *hex)
{
    size_t i;
    const char *digits = skip_hex_prefix(hex);

    if (strlen(digits) != 2 * out_len) return 0;
    for (i = 0; i < out_len; i++) {
        int hi = hex_nibble(digits[2 * i]);
        int lo = hex_nibble(digits[2 * i + 1]);
        if (hi < 0 || lo < 0) return 0;
        out[i] = (uint8_t)((hi << 4) | lo);
    }
    return 1;
}

static int decode_alloc(uint8_t **out, size_t *out_len, const char *hex)
{
    size_t i;
    size_t hex_len;
    uint8_t *decoded;
    const char *digits = skip_hex_prefix(hex);

    hex_len = strlen(digits);
    if ((hex_len & 1) != 0 || hex_len / 2 > MAX_SIGNATURE_BYTES) return 0;
    *out_len = hex_len / 2;
    decoded = malloc(*out_len == 0 ? 1 : *out_len);
    if (decoded == NULL) return 0;

    for (i = 0; i < *out_len; i++) {
        int hi = hex_nibble(digits[2 * i]);
        int lo = hex_nibble(digits[2 * i + 1]);
        if (hi < 0 || lo < 0) {
            free(decoded);
            return 0;
        }
        decoded[i] = (uint8_t)((hi << 4) | lo);
    }
    *out = decoded;
    return 1;
}

int main(int argc, char **argv)
{
    slh_param_t prm;
    uint8_t pk[2 * PUBLIC_KEY_COMPONENT_BYTES];
    uint8_t message[MESSAGE_BYTES];
    uint8_t wrapped[2 + MESSAGE_BYTES] = {0};
    uint8_t *signature = NULL;
    size_t signature_len = 0;
    int external;
    int internal_wrapped;
    int internal_raw;

    if (argc != 5 ||
        !decode_exact(pk, PUBLIC_KEY_COMPONENT_BYTES, argv[1]) ||
        !decode_exact(pk + PUBLIC_KEY_COMPONENT_BYTES, PUBLIC_KEY_COMPONENT_BYTES, argv[2]) ||
        !decode_exact(message, sizeof message, argv[3]) ||
        !decode_alloc(&signature, &signature_len, argv[4])) {
        fprintf(stderr, "usage: %s PK_SEED16 PK_ROOT16 MESSAGE32 SIGNATURE_HEX\n", argv[0]);
        return 2;
    }

    /* SYSCOIN: Reuse only the standard SHA2-128s hash/address function
     * pointers; the SP 800-230 IPD tuple is instantiated explicitly here. */
    memcpy(&prm, &slh_dsa_sha2_128s, sizeof prm);
    prm.alg_id = "SLH-DSA-SHA2-128-24-SP800-230-IPD";
    prm.n = 16;
    prm.h = 22;
    prm.d = 1;
    prm.hp = 22;
    prm.a = 24;
    prm.k = 6;
    prm.lg_w = 2;
    prm.m = 21;

    if (slh_pk_sz(&prm) != sizeof pk || slh_sig_sz(&prm) != 3856) {
        fputs("parameter adaptation produced unexpected sizes\n", stderr);
        free(signature);
        return 3;
    }

    /* SYSCOIN: FIPS 205 external empty-context mode is equivalent to the
     * internal interface over M' = 0x00 || 0x00 || M. Check both routes. */
    memcpy(wrapped + 2, message, sizeof message);
    external = slh_verify(message, sizeof message, signature, signature_len,
                          NULL, 0, pk, &prm);
    internal_wrapped = slh_verify_internal(wrapped, sizeof wrapped, signature,
                                           signature_len, pk, &prm);
    internal_raw = slh_verify_internal(message, sizeof message, signature,
                                       signature_len, pk, &prm);

    printf("{\"external\":%d,\"internalWrapped\":%d,\"internalRaw\":%d}\n",
           external, internal_wrapped, internal_raw);
    free(signature);
    return 0;
}
