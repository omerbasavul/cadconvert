"""Take a .usdc apart.

Written to learn the crate format from files USD itself produced, and kept
because it is how a file this project writes is checked against one it did not.
Run it over both and the sections line up or they do not.

    python3 tools/usdc_decode.py file.usdc
"""

import struct, sys
from lz4 import pxr_decompress

def ints(buf, count, width=4):
    """Usd_IntegerCompression: a common value, two bits of code per integer,
    then a variable-width delta for each code that is not the common one."""
    raw = pxr_decompress(buf, None)
    fmt = '<i' if width == 4 else '<q'
    common = struct.unpack_from(fmt, raw, 0)[0]
    at = width
    ncodes = (count + 3) // 4
    codes = raw[at:at+ncodes]; at += ncodes
    out = []; prev = 0
    for i in range(count):
        code = (codes[i >> 2] >> (2 * (i & 3))) & 3
        if code == 0:
            d = common
        elif code == 1:
            d = struct.unpack_from('<b', raw, at)[0]; at += 1
        elif code == 2:
            d = struct.unpack_from('<h', raw, at)[0]; at += 2
        else:
            d = struct.unpack_from(fmt, raw, at)[0]; at += width
        prev += d
        out.append(prev)
    return out

def main(path):
    b = open(path,'rb').read()
    toc = struct.unpack_from('<q', b, 16)[0]
    n = struct.unpack_from('<q', b, toc)[0]
    secs = {}
    at = toc + 8
    for _ in range(n):
        name = b[at:at+16].split(b'\0')[0].decode()
        s, z = struct.unpack_from('<qq', b, at+16); secs[name] = (s, z); at += 32

    s, z = secs['TOKENS']
    cnt, unc, comp = struct.unpack_from('<qqq', b, s)
    tokens = [t.decode() for t in pxr_decompress(b[s+24:s+24+comp], unc).split(b'\0')][:cnt]
    print(f"TOKENS ({cnt}):"); print("  ", tokens)

    s, z = secs['STRINGS']
    cnt = struct.unpack_from('<q', b, s)[0]
    strings = list(struct.unpack_from(f'<{cnt}I', b, s+8)) if cnt else []
    print(f"STRINGS ({cnt}):", [tokens[i] for i in strings])

    s, z = secs['FIELDS']
    cnt = struct.unpack_from('<q', b, s)[0]
    at = s + 8
    csize = struct.unpack_from('<q', b, at)[0]; at += 8
    field_tokens = ints(b[at:at+csize], cnt); at += csize
    comp = struct.unpack_from('<q', b, at)[0]; at += 8
    reps = struct.unpack(f'<{cnt}Q', pxr_decompress(b[at:at+comp], cnt*8))
    print(f"FIELDS ({cnt}):")
    for t, r in zip(field_tokens, reps):
        ty = (r >> 48) & 0xFF
        print(f"   {tokens[t]:<22} type={ty:<3} inline={(r>>63)&1} array={(r>>61)&1} comp={(r>>62)&1} payload={r & ((1<<48)-1)}")

    s, z = secs['FIELDSETS']
    cnt = struct.unpack_from('<q', b, s)[0]
    csize = struct.unpack_from('<q', b, s+8)[0]
    fs = ints(b[s+16:s+16+csize], cnt)
    print(f"FIELDSETS ({cnt}):", fs)

    s, z = secs['PATHS']
    npaths, nenc = struct.unpack_from('<qq', b, s)
    at = s + 16
    arrs = []
    for _ in range(3):
        csize = struct.unpack_from('<q', b, at)[0]; at += 8
        arrs.append(ints(b[at:at+csize], nenc)); at += csize
    print(f"PATHS ({npaths}, {nenc} encoded):")
    print("   indexes:", arrs[0]); print("   elements:", arrs[1]); print("   jumps:", arrs[2])

    s, z = secs['SPECS']
    cnt = struct.unpack_from('<q', b, s)[0]
    at = s + 8
    arrs = []
    for _ in range(3):
        csize = struct.unpack_from('<q', b, at)[0]; at += 8
        arrs.append(ints(b[at:at+csize], cnt)); at += csize
    print(f"SPECS ({cnt}):")
    print("   paths:", arrs[0]); print("   fieldsets:", arrs[1]); print("   types:", arrs[2])

main(sys.argv[1])
