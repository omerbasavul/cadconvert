def lz4_block_decompress(src, expected=None):
    out = bytearray(); i = 0
    while i < len(src):
        tok = src[i]; i += 1
        lit = tok >> 4
        if lit == 15:
            while True:
                b = src[i]; i += 1; lit += b
                if b != 255: break
        out += src[i:i+lit]; i += lit
        if i >= len(src): break
        off = src[i] | (src[i+1] << 8); i += 2
        ml = tok & 0xF
        if ml == 15:
            while True:
                b = src[i]; i += 1; ml += b
                if b != 255: break
        ml += 4
        start = len(out) - off
        for k in range(ml):
            out.append(out[start + k])
    return bytes(out)

def pxr_decompress(buf, uncompressed_size):
    """pxr wraps an LZ4 block (or several) behind a chunk count byte."""
    n = buf[0]
    if n == 0:
        return lz4_block_decompress(buf[1:])
    out = bytearray(); at = 1
    for _ in range(n):
        size = int.from_bytes(buf[at:at+4], 'little'); at += 4
        out += lz4_block_decompress(buf[at:at+size]); at += size
    return bytes(out)
