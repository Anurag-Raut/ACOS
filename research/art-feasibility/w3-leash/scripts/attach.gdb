set pagination off
set confirm off
set breakpoint pending on
set sysroot /home/Qui/Projects/acos/.cache/art/sysroot
set solib-search-path /home/Qui/Projects/acos/.cache/art/sysroot/system/lib64
python
import gdb, struct

ROOT = '/home/Qui/Projects/acos'
LOG = open(ROOT + '/.cache/w3/trace.txt', 'w', buffering=1)

def peek(addr, n):
    return bytes(gdb.selected_inferior().read_memory(addr, n))

def u64(addr):
    return struct.unpack('<Q', peek(addr, 8))[0]

dumped = {'n': 0}

class BP(gdb.Breakpoint):
    def __init__(self, spec, label, min_size=0):
        super().__init__(spec, internal=False)
        self.label = label
        self.min_size = min_size
        self.silent = True
    def stop(self):
        try:
            this = int(gdb.parse_and_eval('$rdi'))
            size = u64(this + 0x10)
            pos = u64(this + 0x20)
            nobj = u64(this + 0x30)
            nxt = u64(this + 0x40)
        except Exception as e:
            LOG.write('W3 %-24s <unreadable: %s>\n' % (self.label, e))
            return False
        if size < self.min_size:
            return False
        try:
            data = peek(u64(this + 0x08), size + 64)
            words = struct.unpack('<8I', data[pos:pos + 32])
        except Exception:
            data = b''
            words = ()
        LOG.write('W3 %-24s pos=%5d size=%5d objs=%2d next=%2d words=%s\n' % (
            self.label, pos, size, nobj, nxt,
            ' '.join('%08x' % w for w in words)))
        if self.label == 'SurfaceControl::readFromParcel':
            dumped['n'] += 1
            if dumped['n'] == 1 and data:
                with open(ROOT + '/.cache/w3/guest-reply.hex', 'w') as f:
                    for off in range(0, size, 16):
                        f.write('%04x: %s\n' % (off, ' '.join('%02x' % b for b in data[off:off + 16])))
                    objs = u64(this + 0x28)
                    f.write('objects:')
                    for i in range(nobj):
                        f.write(' %d' % u64(objs + 8 * i))
                    f.write('\n')
        return False

BP('_ZN7android14SurfaceControl14readFromParcelERKNS_6ParcelEPNS_2spIS0_EE',
   'SurfaceControl::readFromParcel')
BP('_ZNK7android6Parcel10readObjectEb', 'Parcel::readObject')
BP('_ZNK7android6Parcel24readNullableStrongBinderEPNS_2spINS_7IBinderEEE',
   'readNullableStrongBinder', 2000)
BP('_ZNK7android6Parcel21finishUnflattenBinderERKNS_2spINS_7IBinderEEEPS3_',
   'finishUnflattenBinder', 2000)
BP('_ZNK7android6Parcel12readString16Ev', 'readString16', 2000)
BP('_ZNK7android6Parcel11readString8Ev', 'readString8', 2000)
BP('_ZNK7android6Parcel9readInt32Ev', 'readInt32', 2000)
BP('_ZNK7android6Parcel9readInt32EPi', 'readInt32p', 2000)
LOG.write('W3 breakpoints installed\n')
end
echo W3 attached, continuing\n
continue
