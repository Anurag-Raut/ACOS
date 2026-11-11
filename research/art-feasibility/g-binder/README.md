# g-binder — userspace binder driver for stock libbinder (E3G)

See `results/art-feasibility/E3G.md` for the result, the wire details and the caveats.

    scripts/build.sh    # NDK clang: native/binder_driver.c ; rustc: peer/src/main.rs ; javac+d8: java/
    scripts/run.sh all  # smgr | echo | fd | latency | reverse (or one rung)

    E3G_DEBUG=1 scripts/run.sh all   # driver trace on stderr
    E3G_HEX=1   scripts/run.sh echo  # peer-side parcel hexdump

`peer.log` lands in `.cache/art-exp-g/logs/`. The test dex runs on `dalvikvm64` inside the
E1A/E2D mini-root (`scripts/e3groot.sh` = `e2droot.sh` + `E3G_SOCK`/`E3G_DEBUG`), with
`libbindere3g.so` as an extra `LD_PRELOAD`.
