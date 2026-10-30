"""Durable single-flight transaction primitives for the zkSYS membership keeper.

The caller must hold one exclusive lock for the journal and signers throughout
all operations. RPC functions have the signature ``rpc(method, params)``.
Signed transactions are sensitive spend authorizations; journal storage must be
owner-only, durable, and bound to the deployment and signer identities.
"""

import re
import time

_ADDRESS = re.compile(r"0x[0-9a-fA-F]{40}\Z")
_HASH = re.compile(r"0x[0-9a-fA-F]{64}\Z")
_RAW = re.compile(r"0x(?:[0-9a-fA-F]{2})+\Z")


class TransactionError(RuntimeError):
    pass


class FinalizedRevert(TransactionError):
    def __init__(self, receipt):
        super().__init__("transaction reverted")
        self.receipt = receipt


def _quantity(value):
    if isinstance(value, bool):
        raise TransactionError("invalid numeric value")
    try:
        number = int(value, 16) if isinstance(value, str) and value.startswith("0x") else int(value)
    except (TypeError, ValueError) as exc:
        raise TransactionError("invalid numeric value") from exc
    if number < 0:
        raise TransactionError("negative numeric value")
    return number


def _address(value):
    if not isinstance(value, str) or not _ADDRESS.fullmatch(value):
        raise TransactionError("invalid address")
    return value.lower()


def _hash(value):
    if not isinstance(value, str) or not _HASH.fullmatch(value):
        raise TransactionError("invalid hash")
    return value.lower()


def _same_transaction(expected, actual):
    # The decoder must recover the sender from the signature, not trust an RPC
    # supplied `from`. Check every economically meaningful field before sending.
    for key in ("from", "to"):
        if _address(expected[key]) != _address(actual[key]):
            return False
    for key in ("chainId", "nonce", "value", "gas", "gasPrice"):
        if _quantity(expected[key]) != _quantity(actual[key]):
            return False
    for key in ("data",):
        if str(expected[key]).lower() != str(actual[key]).lower():
            return False
    return True


class TransactionDriver:
    """Single in-flight tx per chain; caller supplies a durable journal writer.

    ``sign(tx)`` returns signed raw hex; ``decode(raw)`` must recover signer and
    transaction fields, while ``hash_raw(raw)`` must return the Ethereum keccak
    hash. The journal's ``transactions`` dictionary must be persisted atomically
    with fsync by ``save(journal)``; it must survive process restarts.
    """

    def __init__(self, journal, save, sign, decode, hash_raw, clock=time.time):
        self.journal = journal
        self.save = save
        self.sign = sign
        self.decode = decode
        self.hash_raw = hash_raw
        self.clock = clock
        self.journal.setdefault("transactions", {})
        self.poisoned = False

    def _check(self):
        if self.poisoned:
            raise TransactionError("journal persistence failed; restart and reconcile")

    def _save(self):
        try:
            self.save(self.journal)
        except BaseException:
            # The in-memory state may no longer match the durable state. Never
            # allocate another nonce in this process after an uncertain write.
            self.poisoned = True
            raise

    def begin(self, name, rpc, chain_id, sender, to, data, value, gas, gas_price, *, kind, metadata=None):
        self._check()
        if kind not in ("bridge", "activation"):
            raise TransactionError("invalid transaction kind")
        if name in self.journal["transactions"]:
            raise TransactionError("chain already has an unresolved transaction")
        if _quantity(rpc("eth_chainId", [])) != _quantity(chain_id):
            raise TransactionError("unexpected chain id")
        sender, to = _address(sender), _address(to)
        for existing in self.journal["transactions"].values():
            if existing["chainId"] == _quantity(chain_id) and existing["sender"] == sender:
                raise TransactionError("signer already has an unresolved transaction")
        if not isinstance(data, str) or not re.fullmatch(r"0x(?:[0-9a-fA-F]{2})*", data):
            raise TransactionError("invalid calldata")
        latest = _quantity(rpc("eth_getTransactionCount", [sender, "latest"]))
        pending = _quantity(rpc("eth_getTransactionCount", [sender, "pending"]))
        if latest != pending:
            raise TransactionError("signer has pending transactions")
        tx = {"from": sender, "to": to, "data": data.lower(), "chainId": _quantity(chain_id),
              "nonce": latest, "value": _quantity(value), "gas": _quantity(gas),
              "gasPrice": _quantity(gas_price), "type": 0}
        if not tx["gas"] or not tx["gasPrice"]:
            raise TransactionError("gas and gas price must be positive")
        raw = self.sign(dict(tx))
        if not isinstance(raw, str) or not _RAW.fullmatch(raw):
            raise TransactionError("invalid signed transaction")
        decoded = self.decode(raw)
        if not _same_transaction(tx, decoded) or _quantity(decoded.get("type", 0)) != 0:
            raise TransactionError("signed transaction does not match request")
        digest = _hash(self.hash_raw(raw))
        entry = {"kind": kind, "chainId": tx["chainId"], "sender": sender, "nonce": latest,
                 "txHash": digest, "raw": raw.lower(), "state": "signed", "metadata": metadata or {}}
        self.journal["transactions"][name] = entry
        # Commit signed bytes and their identity before any network submission.
        self._save()
        return self.broadcast(name, rpc)

    def broadcast(self, name, rpc):
        self._check()
        entry = self.journal["transactions"][name]
        if entry["state"] not in ("signed", "submitted"):
            raise TransactionError("transaction is not broadcastable")
        if _quantity(rpc("eth_chainId", [])) != entry["chainId"]:
            raise TransactionError("unexpected chain id")
        if _hash(self.hash_raw(entry["raw"])) != entry["txHash"]:
            raise TransactionError("journal transaction hash mismatch")
        # A timeout leaves the journal in its existing state. Only the exact
        # persisted bytes may be retried; a replacement needs explicit handling.
        try:
            response = rpc("eth_sendRawTransaction", [entry["raw"]])
        except Exception:
            # RPCs may report "already known" after a timeout or concurrent
            # inclusion. Trust only a lookup of this exact transaction hash.
            receipt = rpc("eth_getTransactionReceipt", [entry["txHash"]])
            if receipt is not None:
                if not isinstance(receipt, dict) or _hash(receipt.get("transactionHash")) != entry["txHash"]:
                    raise TransactionError("receipt transaction hash mismatch")
            else:
                known = rpc("eth_getTransactionByHash", [entry["txHash"]])
                if known is None:
                    raise
                if not isinstance(known, dict) or _hash(known.get("hash")) != entry["txHash"]:
                    raise TransactionError("transaction lookup hash mismatch")
            response = entry["txHash"]
        result = _hash(response)
        if result != entry["txHash"]:
            raise TransactionError("RPC returned a different transaction hash")
        entry["state"] = "submitted"
        self._save()
        return result

    def receipt(self, name, rpc):
        self._check()
        entry = self.journal["transactions"][name]
        if _quantity(rpc("eth_chainId", [])) != entry["chainId"]:
            raise TransactionError("unexpected chain id")
        receipt = finalized_receipt(rpc, entry["txHash"])
        if receipt is None:
            if entry["state"] in ("finalized", "failed"):
                raise TransactionError("finalized transaction receipt disappeared or lost finality")
            return None
        if entry["state"] in ("finalized", "failed") and "receiptBlockHash" in entry and (
            _hash(receipt["blockHash"]) != entry["receiptBlockHash"]
            or _quantity(receipt["blockNumber"]) != entry["receiptBlockNumber"]
        ):
            raise TransactionError("finalized transaction receipt changed")
        if _quantity(receipt.get("status")) != 1:
            entry["state"] = "failed"
            entry["receiptBlockHash"] = _hash(receipt["blockHash"])
            entry["receiptBlockNumber"] = _quantity(receipt["blockNumber"])
            self._save()
            raise FinalizedRevert(receipt)
        if entry["state"] == "failed":
            raise TransactionError("finalized transaction status changed")
        entry["state"] = "finalized"
        entry["receiptBlockHash"] = _hash(receipt["blockHash"])
        entry["receiptBlockNumber"] = _quantity(receipt["blockNumber"])
        self._save()
        return receipt

    def rebroadcast_if_missing(self, name, rpc):
        """Retry only the original signed bytes when the transaction is absent."""
        self._check()
        entry = self.journal["transactions"][name]
        if entry["state"] not in ("signed", "submitted"):
            raise TransactionError("transaction is not eligible for rebroadcast")
        if _quantity(rpc("eth_chainId", [])) != entry["chainId"]:
            raise TransactionError("unexpected chain id")
        # receipt() also returns None for an included transaction awaiting
        # finality; never resubmit merely because finality has not advanced.
        if rpc("eth_getTransactionReceipt", [entry["txHash"]]) is not None:
            return False
        known = rpc("eth_getTransactionByHash", [entry["txHash"]])
        if known is not None:
            if not isinstance(known, dict) or _hash(known.get("hash")) != entry["txHash"]:
                raise TransactionError("transaction lookup hash mismatch")
            return False
        now = _quantity(int(self.clock()))
        next_retry = _quantity(entry.get("nextRetryAt", 0))
        if now < next_retry:
            return False
        latest = _quantity(rpc("eth_getTransactionCount", [entry["sender"], "latest"]))
        pending = _quantity(rpc("eth_getTransactionCount", [entry["sender"], "pending"]))
        if latest > entry["nonce"] or pending > entry["nonce"]:
            raise TransactionError("missing transaction nonce already consumed or pending")
        if latest != entry["nonce"] or pending != entry["nonce"]:
            raise TransactionError("signer nonce is behind the journal")
        if rpc("eth_getTransactionReceipt", [entry["txHash"]]) is not None:
            return False
        backoff = _quantity(entry.get("retryBackoff", 0))
        if backoff > 4:
            raise TransactionError("invalid retry backoff")
        entry["nextRetryAt"] = now + min(600, 60 * (2 ** backoff))
        entry["retryBackoff"] = min(backoff + 1, 4)
        # Persist the retry deadline before broadcast so restarts and uncertain
        # RPC responses cannot bypass the rate limit.
        self._save()
        self.broadcast(name, rpc)
        return True

    def complete(self, name):
        self._check()
        entry = self.journal["transactions"][name]
        if entry["state"] != "finalized":
            raise TransactionError("transaction is not finalized")
        if entry["kind"] == "bridge" and not entry.get("l2Finalized"):
            raise TransactionError("bridge operation is not finalized on L2")
        del self.journal["transactions"][name]
        self._save()

    def supersede_activation(self, name, rpc, verify):
        """Archive a finalized failed activation only after its goal is satisfied."""
        self._check()
        entry = self.journal["transactions"][name]
        if entry["kind"] != "activation" or entry["state"] != "failed":
            raise TransactionError("transaction is not a failed activation")
        if _quantity(rpc("eth_chainId", [])) != entry["chainId"]:
            raise TransactionError("unexpected chain id")
        receipt = finalized_receipt(rpc, entry["txHash"])
        if (receipt is None or _quantity(receipt.get("status")) != 0
                or _hash(receipt["blockHash"]) != entry["receiptBlockHash"]
                or _quantity(receipt["blockNumber"]) != entry["receiptBlockNumber"]):
            raise TransactionError("finalized failed activation receipt changed")
        if verify(receipt, entry) is not True:
            raise TransactionError("failed activation not independently satisfied")
        again = finalized_receipt(rpc, entry["txHash"])
        if (again is None or _quantity(again.get("status")) != 0
                or _hash(again["blockHash"]) != entry["receiptBlockHash"]
                or _quantity(again["blockNumber"]) != entry["receiptBlockNumber"]):
            raise TransactionError("finalized failed activation receipt changed")
        history = self.journal.setdefault("superseded_activations", {})
        if type(history) is not dict or entry["txHash"] in history:
            raise TransactionError("invalid superseded activation history")
        history[entry["txHash"]] = {
            "account": _address(entry["metadata"]["account"]),
            "target": _quantity(entry["metadata"]["target"]),
            "receiptBlockHash": entry["receiptBlockHash"],
            "receiptBlockNumber": entry["receiptBlockNumber"],
        }
        del self.journal["transactions"][name]
        self._save()

    def confirm_bridge_delivery(self, name, l2_rpc, l2_chain_id, canonical_hash, verify_delivery):
        self._check()
        entry = self.journal["transactions"][name]
        if entry["kind"] != "bridge" or entry["state"] != "finalized":
            raise TransactionError("L1 bridge transaction is not finalized")
        if _quantity(l2_rpc("eth_chainId", [])) != _quantity(l2_chain_id):
            raise TransactionError("unexpected L2 chain id")
        canonical_hash = _hash(canonical_hash)
        previous = entry.get("canonicalHash")
        if previous is not None and previous != canonical_hash:
            raise TransactionError("priority operation hash changed")
        entry["canonicalHash"] = canonical_hash
        self._save()
        receipt = finalized_receipt(l2_rpc, canonical_hash)
        if receipt is None:
            return None
        if _quantity(receipt.get("status")) != 1:
            raise TransactionError("L2 priority operation reverted")
        if verify_delivery(receipt, entry) is not True:
            raise TransactionError("L2 priority operation state verification failed")
        entry["l2Finalized"] = True
        entry["l2ReceiptBlockHash"] = _hash(receipt["blockHash"])
        self._save()
        return receipt


def bridge_priority_hash(receipt, bridge, event_topic):
    """Extract the priority hash from exactly one authenticated bridge event."""
    bridge, event_topic = _address(bridge), _hash(event_topic)
    matches = []
    for log in receipt.get("logs", []):
        if log.get("removed"):
            continue
        if _address(log.get("address")) != bridge:
            continue
        topics = log.get("topics", [])
        if len(topics) == 2 and _hash(topics[0]) == event_topic:
            matches.append(_hash(topics[1]))
    if len(matches) != 1:
        raise TransactionError("expected one bridge priority event")
    return matches[0]


def finalized_receipt(rpc, tx_hash):
    """Return a canonical, finalized receipt, or None while still pending."""
    tx_hash = _hash(tx_hash)
    receipt = rpc("eth_getTransactionReceipt", [tx_hash])
    if receipt is None:
        return None
    if _hash(receipt.get("transactionHash")) != tx_hash:
        raise TransactionError("receipt transaction hash mismatch")
    height = _quantity(receipt.get("blockNumber"))
    block_hash = _hash(receipt.get("blockHash"))
    canonical = rpc("eth_getBlockByNumber", [hex(height), False])
    if canonical is None or _hash(canonical.get("hash")) != block_hash:
        raise TransactionError("receipt block is not canonical")
    finalized = rpc("eth_getBlockByNumber", ["finalized", False])
    if finalized is None or finalized.get("number") is None:
        raise TransactionError("finality unavailable")
    if _quantity(finalized["number"]) < height:
        return None
    # Recheck the canonical block after reading finality; an earlier reorg must
    # not turn a stale receipt into a finalized operation.
    canonical = rpc("eth_getBlockByNumber", [hex(height), False])
    if canonical is None or _hash(canonical.get("hash")) != block_hash:
        raise TransactionError("receipt block changed during finality check")
    return receipt


def priority_payload(receipt, emitter, event_topic, canonical_hash, hash_bytes):
    """Authenticate and decode NewPriorityRequest data from the finalized receipt.

    The trusted chain RPC supplies the receipt and canonical block. The event
    emitter must be the independently attested chain diamond. ``hash_bytes``
    returns the Ethereum Keccak-256 hash of bytes, as 0x-prefixed hex.
    """
    emitter, event_topic, canonical_hash = _address(emitter), _hash(event_topic), _hash(canonical_hash)
    matching = []
    for log in receipt.get("logs", []):
        if log.get("removed") or _address(log.get("address")) != emitter:
            continue
        topics = log.get("topics", [])
        if len(topics) == 1 and _hash(topics[0]) == event_topic:
            matching.append(log)
    if len(matching) != 1:
        raise TransactionError("expected one authenticated priority request event")
    data = matching[0].get("data")
    if not isinstance(data, str) or not re.fullmatch(r"0x(?:[0-9a-fA-F]{2})+", data):
        raise TransactionError("invalid priority request event")
    data = bytes.fromhex(data[2:])
    if len(data) < 160 or len(data) % 32:
        raise TransactionError("invalid priority request length")
    def word_at(offset):
        if offset < 0 or offset % 32 or offset + 32 > len(data):
            raise TransactionError("invalid priority request offset")
        return int.from_bytes(data[offset:offset + 32], "big")
    if "0x" + data[32:64].hex() != canonical_hash:
        raise TransactionError("priority request hash differs from bridge event")
    tx_start, deps_start = word_at(96), word_at(128)
    if tx_start != 160 or deps_start <= tx_start + 19 * 32 or deps_start > len(data) - 32:
        raise TransactionError("invalid priority tuple bounds")
    tx = data[tx_start:deps_start]
    if tx[32:44] != bytes(12) or tx[64:76] != bytes(12):
        raise TransactionError("priority address exceeds 160 bits")
    # abi.encode(transaction) has one dynamic top-level tuple offset followed
    # by the exact event tuple encoding. Recompute its canonical hash.
    if _hash(hash_bytes((32).to_bytes(32, "big") + tx)) != canonical_hash:
        raise TransactionError("priority tuple hash mismatch")
    payload_start = tx_start + word_at(tx_start + 14 * 32)
    if payload_start < tx_start + 19 * 32 or payload_start >= deps_start:
        raise TransactionError("invalid priority calldata offset")
    size = word_at(payload_start)
    end = payload_start + 32 + size
    if end > deps_start:
        raise TransactionError("invalid priority calldata length")
    return {
        "from": "0x" + tx[32 + 12:64].hex(),
        "to": "0x" + tx[64 + 12:96].hex(),
        "input": "0x" + data[payload_start + 32:end].hex(),
        "transactionType": int.from_bytes(tx[:32], "big"),
    }
