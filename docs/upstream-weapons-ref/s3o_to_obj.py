"""Minimal .s3o → .obj converter for Spring/Recoil unit models.

S3O format (little-endian):
  header (52 bytes): char magic[12]; u32 version; f32 radius,height,midx,midy,midz;
                     u32 rootPieceOffset, collisionData, tex1_off, tex2_off;
  Each piece: u32 nameOff, numChildren, childrenOff, numVerts, vertsOff,
              vertexType, primitiveType, vertexTableSize, vertexTableOff,
              collisionOff; f32 xoff, yoff, zoff;
  Each vertex: 8 * f32 = xyz, nxnynz, uv.
  primitiveType: 0=triangles, 1=tri-strip, 2=quads.
  0xFFFFFFFF in the index table separates tri-strips / mesh groups.

Usage: python s3o_to_obj.py foo.s3o [foo.obj]
"""
import struct, sys, os


def read_cstring(buf, off):
    end = buf.index(b"\x00", off)
    return buf[off:end].decode("latin-1")


def parse_piece(buf, off, piece_xyz_parent, verts_out, faces_out, uvs_out, name_prefix=""):
    (name_off, n_children, children_off, n_verts, verts_off, vtype, ptype,
     idx_size, idx_off, coll_off, xoff, yoff, zoff) = struct.unpack_from(
        "<10I3f", buf, off)
    name = read_cstring(buf, name_off) if name_off else ""
    full_name = f"{name_prefix}{name}" if name else name_prefix

    # World position accumulates through the piece tree
    px = piece_xyz_parent[0] + xoff
    py = piece_xyz_parent[1] + yoff
    pz = piece_xyz_parent[2] + zoff

    base_index = len(verts_out)
    for vi in range(n_verts):
        vx, vy, vz, nx, ny, nz, u, v = struct.unpack_from(
            "<8f", buf, verts_off + vi * 32)
        verts_out.append((vx + px, vy + py, vz + pz))
        uvs_out.append((u, v))

    indices = list(struct.unpack_from(f"<{idx_size}I", buf, idx_off))
    # Convert indices to faces based on primitive type
    def add_tri(a, b, c):
        if a == b or b == c or a == c:
            return
        faces_out.append((full_name, a + base_index + 1,
                          b + base_index + 1,
                          c + base_index + 1))

    if ptype == 0:  # triangles
        for i in range(0, len(indices) - 2, 3):
            a, b, c = indices[i:i + 3]
            if 0xFFFFFFFF in (a, b, c):
                continue
            add_tri(a, b, c)
    elif ptype == 1:  # tri-strip with 0xFFFFFFFF separators
        strip = []
        flip = False
        for idx in indices:
            if idx == 0xFFFFFFFF:
                strip = []
                flip = False
                continue
            strip.append(idx)
            if len(strip) >= 3:
                a, b, c = strip[-3:]
                if flip:
                    add_tri(a, c, b)
                else:
                    add_tri(a, b, c)
                flip = not flip
    elif ptype == 2:  # quads
        for i in range(0, len(indices) - 3, 4):
            a, b, c, d = indices[i:i + 4]
            if 0xFFFFFFFF in (a, b, c, d):
                continue
            add_tri(a, b, c)
            add_tri(a, c, d)
    else:
        raise ValueError(f"unknown primitive type {ptype} in piece {name}")

    # Children
    for ci in range(n_children):
        child_off = struct.unpack_from("<I", buf, children_off + ci * 4)[0]
        parse_piece(buf, child_off, (px, py, pz),
                    verts_out, faces_out, uvs_out,
                    name_prefix=full_name + "/")


def convert(path_in, path_out):
    with open(path_in, "rb") as f:
        buf = f.read()
    magic = buf[:12]
    assert magic.startswith(b"Spring unit"), f"bad magic {magic!r}"
    (version, radius, height, midx, midy, midz, root_off, coll,
     tex1_off, tex2_off) = struct.unpack_from("<I5f4I", buf, 12)
    tex1 = read_cstring(buf, tex1_off) if tex1_off else ""
    tex2 = read_cstring(buf, tex2_off) if tex2_off else ""

    verts, faces, uvs = [], [], []
    parse_piece(buf, root_off, (0.0, 0.0, 0.0), verts, faces, uvs)

    with open(path_out, "w") as f:
        f.write(f"# converted from {os.path.basename(path_in)}\n")
        f.write(f"# radius={radius:.3f} height={height:.3f} mid=({midx:.3f},{midy:.3f},{midz:.3f})\n")
        f.write(f"# texture1={tex1} texture2={tex2}\n")
        f.write(f"# pieces={len({fn for fn,*_ in faces})} verts={len(verts)} faces={len(faces)}\n")
        for x, y, z in verts:
            f.write(f"v {x:.5f} {y:.5f} {z:.5f}\n")
        for u, v in uvs:
            f.write(f"vt {u:.5f} {v:.5f}\n")
        current = None
        for name, a, b, c in faces:
            if name != current:
                f.write(f"o {name or 'root'}\n")
                current = name
            f.write(f"f {a}/{a} {b}/{b} {c}/{c}\n")

    return dict(radius=radius, height=height, mid=(midx, midy, midz),
                tex1=tex1, tex2=tex2, verts=len(verts), faces=len(faces),
                pieces=len({fn for fn, *_ in faces}))


if __name__ == "__main__":
    if len(sys.argv) == 1:
        # batch-convert every .s3o in the script dir
        here = os.path.dirname(os.path.abspath(__file__))
        for fn in sorted(os.listdir(here)):
            if fn.lower().endswith(".s3o"):
                out = os.path.join(here, fn[:-4] + ".obj")
                info = convert(os.path.join(here, fn), out)
                print(f"{fn:<20} -> {os.path.basename(out):<20} {info}")
    else:
        out = sys.argv[2] if len(sys.argv) > 2 else sys.argv[1][:-4] + ".obj"
        print(convert(sys.argv[1], out))
