"""SYSCOIN: Authenticate secp256k1 wallet entries without exposing keys in argv."""

import re
import subprocess

SECP256K1_FIELD = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
SECP256K1_ORDER = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
SECP256K1_GENERATOR = (
    0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798,
    0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8,
)


def normalize_address(value, label):
    if isinstance(value, int) and not isinstance(value, bool):
        value = "0x" + format(value, "040x")
    if not isinstance(value, str):
        raise SystemExit(f"missing or invalid {label}")
    value = value.strip().lower()
    if not re.fullmatch(r"0x[0-9a-f]{40}", value) or int(value[2:], 16) == 0:
        raise SystemExit(f"missing or invalid {label}")
    return value


def normalize_private_key(value, label):
    if isinstance(value, int) and not isinstance(value, bool):
        value = "0x" + format(value, "064x")
    if not isinstance(value, str):
        raise SystemExit(f"missing or invalid {label}")
    value = value.strip().lower()
    if not re.fullmatch(r"0x[0-9a-f]{64}", value):
        raise SystemExit(f"missing or invalid {label}")
    scalar = int(value[2:], 16)
    if not 0 < scalar < SECP256K1_ORDER:
        raise SystemExit(f"invalid secp256k1 scalar in {label}")
    return value


def _point_add(left, right):
    if left is None:
        return right
    if right is None:
        return left
    x1, y1 = left
    x2, y2 = right
    if x1 == x2 and (y1 + y2) % SECP256K1_FIELD == 0:
        return None
    if left == right:
        slope = (3 * x1 * x1) * pow(2 * y1, -1, SECP256K1_FIELD)
    else:
        slope = (y2 - y1) * pow(x2 - x1, -1, SECP256K1_FIELD)
    slope %= SECP256K1_FIELD
    x3 = (slope * slope - x1 - x2) % SECP256K1_FIELD
    return x3, (slope * (x1 - x3) - y1) % SECP256K1_FIELD


def _scalar_multiply(scalar):
    result = None
    addend = SECP256K1_GENERATOR
    while scalar:
        if scalar & 1:
            result = _point_add(result, addend)
        addend = _point_add(addend, addend)
        scalar >>= 1
    return result


def address_for_private_key(value, label, cast_bin):
    private_key = normalize_private_key(value, label)
    point = _scalar_multiply(int(private_key[2:], 16))
    public_key = "0x" + point[0].to_bytes(32, "big").hex() + point[1].to_bytes(32, "big").hex()
    try:
        digest = subprocess.check_output(
            [cast_bin, "keccak", public_key], text=True
        ).strip().lower()
    except (OSError, subprocess.CalledProcessError) as exc:
        raise SystemExit(f"failed to derive address for {label}") from exc
    if not re.fullmatch(r"0x[0-9a-f]{64}", digest):
        raise SystemExit(f"cast returned an invalid public-key hash for {label}")
    return "0x" + digest[-40:]


def authenticate_wallet_entry(entry, label, cast_bin):
    if not isinstance(entry, dict):
        raise SystemExit(f"missing or invalid {label}")
    address = normalize_address(entry.get("address"), f"{label}.address")
    private_key = normalize_private_key(entry.get("private_key"), f"{label}.private_key")
    derived = address_for_private_key(private_key, f"{label}.private_key", cast_bin)
    if derived != address:
        raise SystemExit(
            f"{label} address/private-key mismatch: configured={address} derived={derived}"
        )
    return address, private_key
