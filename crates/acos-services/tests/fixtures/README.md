# Captured real BLAST `setTransactionState` parcels (W8)

These are byte-for-byte captures of two real `android.ui.ISurfaceComposer`
`setTransactionState` (code 8) request parcels produced by the **API-34 image's own**
`libgui` inside the `counter.apk` launch, taken by the opt-in `ACOS_W5_DUMP` path in
`crates/acos-services/src/surface_client.rs` (dump is `data.clone();
set_data_position(0); marshall()`), from run `.cache/w7/run10.out`.

* `blast_txn_1028.bin` — the buffer transaction: `layerId = 3`, `what =
  0x0000000100000040`, and the test layer's first real `GraphicBuffer`
  (1080x2209, RGBA_8888, 1 fd, 9 transport ints).
* `blast_txn_700.bin` — a sibling transaction on `layerId = 4098` with no buffer.

## Layout of the captured prefix (72 bytes, present in both files)

| offset | contents |
|---:|---|
| 0 | `04 00 00 c2` (parcel/binder request word; not decoded by C1) |
| 4 | `ff ff ff ff` |
| 8 | `54 53 59 53` (`TSYS`) |
| 12 | `1b 00 00 00` = 27 |
| 16 | UTF-16 `android.ui.ISurfaceComposer` (27 chars), padded to 72 |

The transaction body therefore starts at **offset 72**; C1's decoder starts there
(the peer consumes the interface token first). Every position quoted in
`results/art-runtime/W8.md` is a *body* offset (`dump_offset - 72`).

## Verified anchors in `blast_txn_1028.bin` (body offsets)

| body off | value | meaning |
|---:|---|---|
| 0 | 28 | `FrameTimelineInfo` size word (counts itself) |
| 28 | 1 | `composerCount` |
| 32 | object | `layer_state_t::surface` strong binder |
| 56 | 3 | `layerId` |
| 60 | `0x100000040` | `what` |
| 316 | end of 16-float identity `colorTransform` | strong anchor for the prefix |
| 472 | 0 | `blurRegions` count |
| 476..516 | zeros | `StretchEffect` (40 bytes, `writeInplace(0x28)`) |
| 516..532 | 0 | `bufferCrop` Rect |
| 532..548 | 0,0,1080,2209 | `destinationFrame` Rect |
| 548 | 0 | `isTrustedOverlay` |
| 552 | 0 | `dropInputMode` |
| 556 | 1 | `hasBufferData` |
| 560 | 1 | `Parcel::writeParcelable` presence marker for `BufferData` |
| 564 | 7 | `BufferData::flags` |
| 568 | 1 | `hasBuffer` |
| 572 | 88 | `GraphicBuffer` flattened size (22 words = 13-header + 9 transport ints) |
| 576 | 1 | fd count |
| 580 | `31 30 42 47` | image `libui` magic (image writes the *reverse* byte order of the tag's `'GB01'`) |
| 584,588,592,596,600 | 1080, 2209, 1080, 1, 1 | width, height, stride, format, layerCount |
| 604,608,612,616,620,624,628 | 2355, 2, 0, 0, 1, 9, 0 | usageLo, idHi, idLo, generation, numFds, numInts, usageHi |
| 632..668 | 1080,2209,1080,1,2355,0,1,4,0 | the 9 transport ints |
| 668..692 | `fd` object | `BINDER_TYPE_FD` (`0x66642a85`), fd 14 |
| 692 | 1 | `hasFence` |
| 696,700,704 | 4,0,0 | `Fence` flatten (`len=4`, no fds, `numFds=0`) |
| 708 | 1 | `frameNumber` |
| 716,740,764 | objects | `releaseBufferListener`, `releaseBufferEndpoint`, `cachedToken` (3 handles) |
| 788 | `0x200000000` | `cachedBufferId` (== `GraphicBuffer.mId`) |
| 796 | 0 | `hasBarrier` |
| 800 | 0 | `barrierFrameNumber` |
| 808 | 1 | `producerId` |

`blast_txn_700.bin` differs from the 1028 one starting at body 40 (different binder
values) and has *no* `GraphicBuffer`; its post-`hasBufferData` region still has to be
matched field by field (see `results/art-runtime/W8.md` §4).

Do not use these fixtures as a "golden output" for the current decoder: at the time of
capture the decoder was misaligned and rejected both parcels. They are input evidence.
