import struct, sys, itertools
names = ["sin", "cos", "tan", "atan2", "asin", "acos", "powf", "hypot", "cbrt", "sqrt", "powi",
         "glam Quat*Vec3", "glam Vec3A.normalize", "glam Vec3A.dot", "glam Affine3A", "glam slerp"]
N = 200000
files = sys.argv[1:]
data = {f: open(f, "rb").read() for f in files}
for a, b in itertools.combinations(files, 2):
    print(f"--- {a} vs {b}")
    for k, name in enumerate(names):
        xa = struct.unpack_from(f"<{N}I", data[a], k * N * 4)
        xb = struct.unpack_from(f"<{N}I", data[b], k * N * 4)
        d = sum(1 for x, y in zip(xa, xb) if x != y)
        print(f"  {name:22} {d:7d} / {N} differ ({100.0 * d / N:.3f}%)")
