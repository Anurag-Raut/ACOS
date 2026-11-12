{
  "samples": 24,
  "fresh_ok": 24,
  "wall": {
    "n": 24,
    "p50": 80.0895,
    "p95": 81.43929999999999,
    "min": 78.98,
    "max": 82.442
  },
  "client_write": {
    "label": "1. client request write (sendall)",
    "kind": "top",
    "us": {
      "n": 24,
      "p50": 5.0,
      "p95": 6.0,
      "min": 4,
      "max": 8
    }
  },
  "client_read": {
    "label": "   client read of the reply",
    "kind": "top",
    "us": {
      "n": 24,
      "p50": 5.0,
      "p95": 6.0,
      "min": 4,
      "max": 7
    }
  },
  "unattributed": {
    "n": 24,
    "p50": 80079.5,
    "p95": 81428.45,
    "min": 78971.0,
    "max": 82430.0
  }
}

### D-M5 phase table

n=24 (fresh 24), one `acos_observe {png:true, screenshot:path}` per sample,
microseconds unless stated, p50 / p95 over the non-warm-up samples.

| phase | p50 | p95 | min | max | n |
|---|---:|---:|---:|---:|---:|
| **wall (request write -> reply read), ms** | **80.09** | **81.44** | 78.98 | 82.44 | 24 |
| 1. client request write (sendall) | 5 | 6 | 4 | 8 | 24 |
|    client read of the reply | 5 | 6 | 4 | 7 | 24 |
| (unattributed: json/parse/probe) | 80079 | 81428 | 78971 | 82430 | 24 |

**C1 composite internals**

| sub-phase | p50 | p95 | min | max | n |
|---|---:|---:|---:|---:|---:|

**PNG encode internals**

| sub-phase | p50 | p95 | min | max | n |
|---|---:|---:|---:|---:|---:|
