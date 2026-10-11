#!/usr/bin/env python3
"""Decode a zisofs-compressed tree (what 7z leaves behind when it extracts a
mkzftree/AppImage-v1 ISO: files that start with the 8-byte magic
37e45396c9dbd607). Usage: zisofs-decode.py <extracted-dir> <out-dir>.
Used to open the 2013 portablelinuxgames Kernel Panic 4.4 package."""
MAGIC = bytes.fromhex("37e45396c9dbd607")
src, dst = sys.argv[1], sys.argv[2]
n_dec = n_copy = 0
for root, dirs, files in os.walk(src):
    for name in files:
        p = os.path.join(root, name)
        rel = os.path.relpath(p, src)
        out = os.path.join(dst, rel)
        os.makedirs(os.path.dirname(out), exist_ok=True)
        if os.path.islink(p):
            continue
        d = open(p, "rb").read()
        if d[:8] != MAGIC:
            open(out, "wb").write(d); n_copy += 1; continue
        size, hdr_len, blk_log2 = struct.unpack_from("<IBB", d, 8)
        hdr = hdr_len * 4
        blk = 1 << blk_log2
        nblocks = (size + blk - 1) // blk
        ptrs = struct.unpack_from("<%dI" % (nblocks + 1), d, hdr)
        res = bytearray()
        for i in range(nblocks):
            a, b = ptrs[i], ptrs[i + 1]
            if a == b:
                res += b"\0" * blk
            else:
                res += zlib.decompress(d[a:b])
        res = bytes(res[:size])
        open(out, "wb").write(res); n_dec += 1
print("decoded", n_dec, "copied", n_copy)
