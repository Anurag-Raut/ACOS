| # | phase | p50 | p95 | share of wall |
|---|---|---:|---:|---:|
| 1 | serve dispatch + tree request serialise | 2.0 µs | 3.0 µs | 0.00% |
| 2a | command write to acos-art stdin | 4.0 µs | 6.0 µs | 0.00% |
| 2b | pipe write -> acos-art reads `tree` | 20.5 µs | 44.1 µs | 0.01% |
| 3 | **settle wait** (150 ms quiet, 15 ms poll) | 151.112 ms | 151.386 ms | 81.06% |
| 4 | **capture side effect** (composite + PNG + write + md5) | 34.468 ms | 35.601 ms | 18.49% |
| 4a |   C1 buffer clone (9,542,880 B rgba) | 12.582 ms | 13.143 ms | 6.75% |
| 4b |   PNG encode (9,545,882 B) | 8.414 ms | 8.940 ms | 4.51% |
| 4c |   PNG file write | 1.158 ms | 1.638 ms | 0.62% |
| 4d |   md5 over the PNG file | 11.514 ms | 12.329 ms | 6.18% |
| 5 | guest probe walk + tree JSON (concurrent; `emit_us`) | <1 ms | <1 ms | 0.00% |
| 6a | art reply build (serde) + write | 26.0 µs | 50.1 µs | 0.01% |
| 6b | client: reply JSON parse | 44.0 µs | 71.7 µs | 0.02% |
| 6c | client: reply tree -> UiTree | 15.0 µs | 26.6 µs | 0.01% |
| 6d | serve: UiTree -> reply JSON | 94.0 µs | 146.9 µs | 0.05% |
| 7a | reply pipe -> client read | 20.5 µs | 37.6 µs | 0.01% |
| 7b | client: read back PNG file (tree reply ignores it) | 428.5 µs | 1.078 ms | 0.23% |
| 8 | serve/client residual (socket in/out, envelope) | 120.0 µs | 1.083 ms | 0.06% |
|  | **total client wall** | 186.416 ms | 187.454 ms |  |

