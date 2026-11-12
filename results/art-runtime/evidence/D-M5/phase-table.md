{
  "samples": 24,
  "fresh_ok": 24,
  "wall": {
    "n": 24,
    "p50": 83.61850000000001,
    "p95": 85.63535,
    "min": 80.731,
    "max": 87.073
  },
  "client_write": {
    "label": "1. client request write (sendall)",
    "kind": "top",
    "us": {
      "n": 24,
      "p50": 5.0,
      "p95": 6.849999999999998,
      "min": 3,
      "max": 7
    }
  },
  "serve_read": {
    "label": "   socket ready -> request line parsed",
    "kind": "top",
    "us": {
      "n": 24,
      "p50": 152.0,
      "p95": 204.54999999999998,
      "min": 97,
      "max": 205
    }
  },
  "serve_dispatch": {
    "label": "2. serve dispatch = handle_request",
    "kind": "top",
    "us": {
      "n": 24,
      "p50": 83573.0,
      "p95": 85599.1,
      "min": 80676,
      "max": 87022
    }
  },
  "art_roundtrip": {
    "label": "3. of which: request to the guest/launcher (acos-art)",
    "kind": "of",
    "us": {
      "n": 24,
      "p50": 82030.5,
      "p95": 84197.75,
      "min": 79229,
      "max": 85601
    }
  },
  "art_composite": {
    "label": "4. of which: C1 composite/capture",
    "kind": "of2",
    "us": {
      "n": 24,
      "p50": 10199.5,
      "p95": 10429.7,
      "min": 9596,
      "max": 10875
    }
  },
  "art_encode": {
    "label": "6. of which: PNG encode",
    "kind": "of2",
    "us": {
      "n": 24,
      "p50": 60920.0,
      "p95": 62445.049999999996,
      "min": 59012,
      "max": 63848
    }
  },
  "art_png_write": {
    "label": "7a. of which: launcher writes the frame file",
    "kind": "of2",
    "us": {
      "n": 24,
      "p50": 924.0,
      "p95": 1172.9499999999998,
      "min": 843,
      "max": 1364
    }
  },
  "art_md5": {
    "label": "8a. of which: md5sum subprocess over the frame",
    "kind": "of2",
    "us": {
      "n": 24,
      "p50": 9631.5,
      "p95": 10170.5,
      "min": 9014,
      "max": 10306
    }
  },
  "art_ipc_remainder": {
    "label": "   of which: pipe + launcher loop remainder",
    "kind": "of2",
    "us": {
      "n": 24,
      "p50": 401.0,
      "p95": 502.2,
      "min": 275,
      "max": 514
    }
  },
  "png_read": {
    "label": "5/8b. of which: serve reads the frame file back",
    "kind": "of",
    "us": {
      "n": 24,
      "p50": 415.5,
      "p95": 484.55,
      "min": 350,
      "max": 529
    }
  },
  "shot_write": {
    "label": "7b. of which: serve writes the caller's PNG",
    "kind": "of",
    "us": {
      "n": 24,
      "p50": 904.0,
      "p95": 1153.95,
      "min": 768,
      "max": 1402
    }
  },
  "serve_reply": {
    "label": "9. reply write",
    "kind": "top",
    "us": {
      "n": 24,
      "p50": 10.0,
      "p95": 10.849999999999998,
      "min": 7,
      "max": 11
    }
  },
  "client_read": {
    "label": "   client read of the reply",
    "kind": "top",
    "us": {
      "n": 24,
      "p50": 4.0,
      "p95": 5.849999999999998,
      "min": 2,
      "max": 6
    }
  },
  "c1_kernel": {
    "label": "   kernel_rows + sfgeom compute",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 7.0,
      "p95": 11.0,
      "min": 6,
      "max": 12
    }
  },
  "c1_memfd_read": {
    "label": "   memfd read (guest buffer -> host Vec)",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 1302.0,
      "p95": 1485.2499999999998,
      "min": 1096,
      "max": 1570
    }
  },
  "c1_blend": {
    "label": "   blend_layer (blit into the canvas)",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 1529.5,
      "p95": 1581.85,
      "min": 1481,
      "max": 1582
    }
  },
  "c1_canvas_alloc": {
    "label": "   canvas allocation",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 226.5,
      "p95": 266.69999999999993,
      "min": 166,
      "max": 334
    }
  },
  "c1_fnv_hash": {
    "label": "   hash_pixels (FNV-1a + non-zero scan)",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 7044.0,
      "p95": 7314.65,
      "min": 6798,
      "max": 7584
    }
  },
  "encode_filter": {
    "label": "   filter-0 row copy",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 220.0,
      "p95": 683.049999999999,
      "min": 134,
      "max": 749
    }
  },
  "encode_stored": {
    "label": "   zlib stored-block framing",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 611.5,
      "p95": 728.1,
      "min": 519,
      "max": 783
    }
  },
  "encode_adler32": {
    "label": "   adler32 (scalar)",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 15801.5,
      "p95": 16355.85,
      "min": 15378,
      "max": 16458
    }
  },
  "encode_idat_chunk": {
    "label": "   IDAT chunk (copy + CRC32 + write)",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 44049.5,
      "p95": 45478.85,
      "min": 42761,
      "max": 46597
    }
  },
  "encode_crc32_idat": {
    "label": "     of which crc32 (bit-serial)",
    "kind": "sub",
    "us": {
      "n": 24,
      "p50": 42964.5,
      "p95": 44411.25,
      "min": 41649,
      "max": 45488
    }
  },
  "unattributed": {
    "n": 24,
    "p50": -117.5,
    "p95": -74.9,
    "min": -170.0,
    "max": -63.0
  }
}

### D-M5 phase table

n=24 (fresh 24), one `acos_observe {png:true, screenshot:path}` per sample,
microseconds unless stated, p50 / p95 over the non-warm-up samples.

| phase | p50 | p95 | min | max | n |
|---|---:|---:|---:|---:|---:|
| **wall (request write -> reply read), ms** | **83.62** | **85.64** | 80.73 | 87.07 | 24 |
| 1. client request write (sendall) | 5 | 6 | 3 | 7 | 24 |
|    socket ready -> request line parsed | 152 | 204 | 97 | 205 | 24 |
| 2. serve dispatch = handle_request | 83573 | 85599 | 80676 | 87022 | 24 |
| 3. of which: request to the guest/launcher (acos-art) | 82030 | 84197 | 79229 | 85601 | 24 |
| 4. of which: C1 composite/capture | 10199 | 10429 | 9596 | 10875 | 24 |
| 6. of which: PNG encode | 60920 | 62445 | 59012 | 63848 | 24 |
| 7a. of which: launcher writes the frame file | 924 | 1172 | 843 | 1364 | 24 |
| 8a. of which: md5sum subprocess over the frame | 9631 | 10170 | 9014 | 10306 | 24 |
|    of which: pipe + launcher loop remainder | 401 | 502 | 275 | 514 | 24 |
| 5/8b. of which: serve reads the frame file back | 415 | 484 | 350 | 529 | 24 |
| 7b. of which: serve writes the caller's PNG | 904 | 1153 | 768 | 1402 | 24 |
| 9. reply write | 10 | 10 | 7 | 11 | 24 |
|    client read of the reply | 4 | 5 | 2 | 6 | 24 |
| (unattributed: json/parse/probe) | -117 | -74 | -170 | -63 | 24 |

**C1 composite internals**

| sub-phase | p50 | p95 | min | max | n |
|---|---:|---:|---:|---:|---:|
|    kernel_rows + sfgeom compute | 7 | 11 | 6 | 12 | 24 |
|    memfd read (guest buffer -> host Vec) | 1302 | 1485 | 1096 | 1570 | 24 |
|    blend_layer (blit into the canvas) | 1529 | 1581 | 1481 | 1582 | 24 |
|    canvas allocation | 226 | 266 | 166 | 334 | 24 |
|    hash_pixels (FNV-1a + non-zero scan) | 7044 | 7314 | 6798 | 7584 | 24 |

**PNG encode internals**

| sub-phase | p50 | p95 | min | max | n |
|---|---:|---:|---:|---:|---:|
|    filter-0 row copy | 220 | 683 | 134 | 749 | 24 |
|    zlib stored-block framing | 611 | 728 | 519 | 783 | 24 |
|    adler32 (scalar) | 15801 | 16355 | 15378 | 16458 | 24 |
|    IDAT chunk (copy + CRC32 + write) | 44049 | 45478 | 42761 | 46597 | 24 |
|      of which crc32 (bit-serial) | 42964 | 44411 | 41649 | 45488 | 24 |
