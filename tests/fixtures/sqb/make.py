"""Writes the hand-built SQL Backup (.sqb) samples the reader's unit tests decode.

They follow the format description in specs/gaps/backup.md, section 6, and are built with
independent implementations (Python's zlib, lzma, hashlib and the `cryptography` package's
AES), not with the reader's own code. No real SQL Backup file was available.

    python tests/fixtures/sqb/make.py

The payload every sample holds is `payload()`; src/sources/backup/sqb.rs builds the same.
"""

import hashlib
import lzma
import os
import struct
import zlib

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

HERE = os.path.dirname(os.path.abspath(__file__))
EXTENT = 16384
PAD = b"4FE7192ABB5C48838EE4F956AD3688C1"


def payload():
    """96 KB: a tape-like start, then text and a little noise so every coder has work."""
    out = bytearray(b"TAPE")
    i = 0
    while len(out) < 6 * EXTENT:
        out += b"row %d of the sample backup, " % i
        out.append((i * 7919) % 251)
        i += 1
    return bytes(out[: 6 * EXTENT])


def ctr(key, counter, data):
    """AES counter mode where the counter is incremented before each block."""
    start = (int.from_bytes(counter, "big") + 1) % (1 << 128)
    enc = Cipher(algorithms.AES(key), modes.CTR(start.to_bytes(16, "big"))).encryptor()
    return enc.update(data) + enc.finalize()


def elf(b):
    h = 0
    for x in b:
        h = ((h << 4) + x) & 0xFFFFFFFF
        high = h & 0xF0000000
        if high:
            h ^= high >> 24
        h &= ~high & 0xFFFFFFFF
    return h


def dec(v):
    return str(v).encode()


def v3_keys(pw):
    h = dec(elf(pw))
    key = h + PAD[: 32 - len(h)]
    c = dec(elf(h))
    return key, c + PAD[: 16 - len(c)]


def chain(seed, n):
    out = dec(elf(seed))
    while len(out) < n:
        out += dec(elf(out))
    return out


def v4_keys(pw, n):
    key = chain(pw, 32)[:n]
    fill = chain(key, 16)
    k = min(len(pw), 16)
    return key, pw[:k] + fill[: 16 - k]


def v5_key(pw, salt, n):
    return hashlib.pbkdf2_hmac("sha256", pw, salt, 1000, n)


def pascal(s):
    return bytes([len(s)]) + s


def put(buf, at, data):
    buf[at : at + len(data)] = data


def lz(data):
    """Level 1 coding: three literals, then control bits in 16-bit words taken when needed."""
    out = bytearray(data[:3])
    state = {"pos": None, "left": 0, "word": 0}

    def bit(b):
        if state["left"] == 0:
            state["pos"] = len(out)
            out.extend(b"\0\0")
            state["left"], state["word"] = 16, 0
        state["left"] -= 1
        state["word"] |= b << state["left"]
        out[state["pos"] : state["pos"] + 2] = struct.pack("<H", state["word"])

    def gamma(v):
        bits = bin(v)[3:]
        for i, c in enumerate(bits):
            bit(int(c))
            bit(1 if i + 1 < len(bits) else 0)

    seen = {}
    i = 3
    while i < len(data):
        best = (0, 0)
        for j in seen.get(data[i : i + 4], [])[-16:]:
            n = 0
            while i + n < len(data) and n < 300 and data[j + n] == data[i + n]:
                n += 1
            if n > best[0]:
                best = (n, i - j)
        for k in range(max(i - 3, 0), i + 1):
            seen.setdefault(data[k : k + 4], []).append(k)
        if best[0] >= 4:
            n, dist = best
            bit(1)
            gamma(n - 2)
            gamma(((dist - 1) >> 8) + 2)
            out.append((dist - 1) & 0xFF)
            i += n
        else:
            bit(0)
            out.append(data[i])
            i += 1
    return bytes(out)


def compress(level, data):
    if level == 0:
        return data
    if level == 1:
        return lz(data)
    if level == 2:
        return zlib.compress(data, 6)
    raw = lzma.compress(data, format=lzma.FORMAT_ALONE)
    return raw[:5] + raw[13:]


def header(version, mode, n, level, devices, sections, pw, salt):
    """The header, encrypted; returns (bytes, key, static counter)."""
    length = 512 if version == 3 else 1024
    h = bytearray(length)
    put(h, 0, pascal(b"SSBE"))
    h[5] = 2 if version == 3 else 4
    put(h, 9, struct.pack("<q", 6 * EXTENT))
    put(h, 184, pascal(b"37000000012800037"))
    h[505] = level
    h[506] = 1
    put(h, 507, pascal(b"SSBE"))
    if version == 3:
        put(h, 33, pascal(b"SERVER"))
        put(h, 131, pascal(b"Sample"))
    else:
        put(h, 294, "SERVER".encode("utf-16-le"))
        put(h, 512, pascal(b"SQB4"))
        put(h, 518, "Sample".encode("utf-16-le"))
        h[981] = 60 if sections else 40
        put(h, 983, salt)
        h[1016] = 1 if devices > 1 else 0
        h[1017] = devices
        put(h, 1019, pascal(b"SQB4"))
    key = counter = None
    if mode == "v3":
        key, counter = v3_keys(pw)
        h[:506] = ctr(key, bytes(16), bytes(h[:506]))
    elif mode == "v4":
        key, counter = v4_keys(pw, n)
        h[:506] = ctr(key, bytes(16), bytes(h[:506]))
        h[518:776] = ctr(key, bytes(16), bytes(h[518:776]))
    elif mode == "v5":
        key = v5_key(pw, salt, n)
        h[:506] = ctr(key, b"\xff" + bytes(15), bytes(h[:506]))
        h[518:776] = ctr(key, b"\xff" + b"\x01" * 15, bytes(h[518:776]))
    return bytes(h), key, counter


def sample(name, version, mode, n, level, devices, sections, pw=b"", salt=bytes(range(16))):
    data = payload()
    h, key, counter = header(version, mode, n, level, devices, sections, pw, salt)
    out = bytearray(h)
    if sections:
        other = b"not the extents"
        out += struct.pack("<iq", 2, 20 + len(other)) + bytes(8) + other
        out += struct.pack("<iq", 1, 0) + bytes(8)
    multi = devices > 1
    # Device d holds every devices-th extent of the payload, interleaved in the file.
    pieces = [data[i : i + EXTENT] for i in range(0, len(data), EXTENT)]
    index = [0] * devices
    for g, piece in enumerate(pieces):
        d = g % devices
        if sections and g == 2:
            out += (b"\xff" + struct.pack("<i", 7) if multi else struct.pack("<ii", -1, 7)) + b"padding"
        body = compress(level, piece)
        if mode != "none":
            c = counter if counter is not None else bytes([d]) + bytes(4) + struct.pack(">q", index[d] + 1) + bytes(3)
            body = ctr(key, c, body)
        index[d] += 1
        out += (bytes([d]) if multi else b"") + struct.pack("<i", len(body))
        if level == 1:
            out += struct.pack("<i", len(piece))
        out += body
    for d in range(devices if multi and sections else 1):
        out += (bytes([d]) if multi else b"") + struct.pack("<i", 0) + (bytes(4) if level == 1 and multi else b"")
    with open(os.path.join(HERE, name), "wb") as f:
        f.write(out)


if __name__ == "__main__":
    sample("plain_zlib.sqb", 4, "none", 0, 2, 1, False)
    sample("v5_aes256_lzma_2devices.sqb", 4, "v5", 32, 4, 2, True, b"Quill!sqb")
    sample("v5_aes128_lz.sqb", 4, "v5", 16, 1, 1, True, b"Quill!sqb")
    sample("v4_aes256_stored.sqb", 4, "v4", 32, 0, 1, False, b"pw")
    sample("v3_aes256_lz.sqb", 3, "v3", 32, 1, 1, False, b"an older password")
    raw = lzma.compress(b"abcabcabcabc hello hello hello" * 40, format=lzma.FORMAT_ALONE)
    with open(os.path.join(HERE, "sample.lzma"), "wb") as f:
        f.write(raw)
